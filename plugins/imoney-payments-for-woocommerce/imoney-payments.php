<?php
/**
 * Plugin Name: Internet Money (IMN) Payments for WooCommerce
 * Plugin URI: https://internetmoneynetwork.org
 * Description: Accept Internet Money (IMN) payments straight to your own address. Each order gets its own invoice number, and your own node confirms the payment on the server. No intermediary holds the money.
 * Version: 2.2.0
 * Author: Internet Money Network Developers
 * Author URI: https://github.com/Internet-Money-Network
 * License: MIT OR Apache-2.0
 * Text Domain: imoney-payments
 */

if (!defined('ABSPATH')) {
    exit; // Exit if accessed directly
}

define('IMONEY_ATOMS_PER_IMN', 100000000);
define('IMONEY_CRON_HOOK', 'imoney_check_pending_orders');

/**
 * Converts a decimal IMN amount to atoms without floating-point rounding.
 */
function imoney_imn_to_atoms($amount) {
    $text = number_format((float) $amount, 8, '.', '');
    list($whole, $fraction) = explode('.', $text);
    return ((int) $whole) * IMONEY_ATOMS_PER_IMN + (int) $fraction;
}

/**
 * Formats atoms as an IMN amount with trailing zeros removed.
 */
function imoney_atoms_to_imn($atoms) {
    $whole = intdiv($atoms, IMONEY_ATOMS_PER_IMN);
    $fraction = rtrim(str_pad((string) ($atoms % IMONEY_ATOMS_PER_IMN), 8, '0', STR_PAD_LEFT), '0');
    return $fraction === '' ? (string) $whole : $whole . '.' . $fraction;
}

/**
 * Returns the gateway instance, or null when WooCommerce is not loaded.
 */
function imoney_gateway() {
    if (!function_exists('WC') || !WC()->payment_gateways()) {
        return null;
    }
    $gateways = WC()->payment_gateways()->payment_gateways();
    return isset($gateways['imoney']) ? $gateways['imoney'] : null;
}

/**
 * Asks the merchant's node what has been paid towards an order's invoice and, when enough has
 * arrived at the required level, marks the order paid. This runs on the server: the customer's
 * browser cannot make an order paid.
 *
 * Returns an array describing the state, for the status endpoint.
 */
function imoney_check_order($order) {
    $state = array('paid' => $order->is_paid(), 'seen' => false, 'included' => false, 'network_alert' => false, 'error' => null);
    if ($state['paid']) {
        return $state;
    }
    $gateway = imoney_gateway();
    $invoice_id = $order->get_meta('_imoney_invoice_id');
    $required_atoms = (int) $order->get_meta('_imoney_amount_atoms');
    $address = $order->get_meta('_imoney_address');
    if (!$gateway || !$invoice_id || $required_atoms <= 0 || !$address) {
        $state['error'] = 'not_an_imoney_order';
        return $state;
    }

    $url = untrailingslashit($gateway->node_url) . '/api/v1/invoice/' . rawurlencode($invoice_id) . '?address=' . rawurlencode($address);
    $response = wp_remote_get($url, array('timeout' => 10));
    if (is_wp_error($response) || wp_remote_retrieve_response_code($response) !== 200) {
        $state['error'] = 'node_unreachable';
        return $state;
    }
    $invoice = json_decode(wp_remote_retrieve_body($response), true);
    if (!is_array($invoice) || !isset($invoice['seen_atoms'], $invoice['included_atoms'], $invoice['final_atoms'])) {
        $state['error'] = 'bad_node_response';
        return $state;
    }

    $state['seen'] = (int) $invoice['seen_atoms'] >= $required_atoms;
    $state['included'] = (int) $invoice['included_atoms'] >= $required_atoms;
    $state['network_alert'] = !empty($invoice['network_alert']);

    $level = in_array($gateway->confirmation_level, array('seen', 'included', 'final'), true) ? $gateway->confirmation_level : 'included';
    if ((int) $invoice[$level . '_atoms'] >= $required_atoms) {
        $tx_id = isset($invoice['payments'][0]['tx_id']) ? sanitize_text_field($invoice['payments'][0]['tx_id']) : '';
        $order->payment_complete($tx_id);
        $order->add_order_note(sprintf(
            /* translators: 1: IMN amount, 2: invoice number, 3: confirmation level */
            __('Internet Money payment of %1$s IMN received for invoice %2$s (level: %3$s).', 'imoney-payments'),
            imoney_atoms_to_imn($required_atoms),
            $invoice_id,
            $level
        ));
        $state['paid'] = true;
    }
    return $state;
}

// Tell WooCommerce this plugin works with its order tables and its block-based checkout
add_action('before_woocommerce_init', 'imoney_declare_compatibility');
function imoney_declare_compatibility() {
    if (class_exists('\Automattic\WooCommerce\Utilities\FeaturesUtil')) {
        \Automattic\WooCommerce\Utilities\FeaturesUtil::declare_compatibility('custom_order_tables', __FILE__, true);
        \Automattic\WooCommerce\Utilities\FeaturesUtil::declare_compatibility('cart_checkout_blocks', __FILE__, true);
    }
}

// Offer the gateway in the block-based checkout, which is the default in current WooCommerce
add_action('woocommerce_blocks_loaded', 'imoney_register_blocks_support');
function imoney_register_blocks_support() {
    if (!class_exists('\Automattic\WooCommerce\Blocks\Payments\Integrations\AbstractPaymentMethodType')) {
        return;
    }

    final class WC_Gateway_IMoney_Blocks extends \Automattic\WooCommerce\Blocks\Payments\Integrations\AbstractPaymentMethodType {
        protected $name = 'imoney';

        public function initialize() {
            $this->settings = get_option('woocommerce_imoney_settings', array());
        }

        public function is_active() {
            $gateway = imoney_gateway();
            return $gateway && $gateway->is_available();
        }

        public function get_payment_method_script_handles() {
            wp_register_script(
                'imoney-blocks',
                plugins_url('blocks.js', __FILE__),
                array('wc-blocks-registry', 'wc-settings', 'wp-element', 'wp-html-entities'),
                '2.2.0',
                true
            );
            return array('imoney-blocks');
        }

        public function get_payment_method_data() {
            return array(
                'title'       => $this->get_setting('title', __('Internet Money (IMN)', 'imoney-payments')),
                'description' => $this->get_setting('description', ''),
                'supports'    => array('products'),
            );
        }
    }

    add_action('woocommerce_blocks_payment_method_type_registration', function ($registry) {
        $registry->register(new WC_Gateway_IMoney_Blocks());
    });
}

// Ensure WooCommerce is active
add_action('plugins_loaded', 'imoney_payments_init_gateway_class');

function imoney_payments_init_gateway_class() {
    if (!class_exists('WC_Payment_Gateway')) {
        return;
    }

    class WC_Gateway_IMoney extends WC_Payment_Gateway {

        public $merchant_address;
        public $node_url;
        public $exchange_rate;
        public $confirmation_level;

        public function __construct() {
            $this->id                 = 'imoney';
            $this->icon               = apply_filters('woocommerce_imoney_icon', '');
            $this->has_fields         = false;
            $this->method_title       = __('Internet Money (IMN)', 'imoney-payments');
            $this->method_description = __('Accept IMN payments directly to your own address, confirmed by your own node.', 'imoney-payments');

            $this->supports = array(
                'products',
            );

            // Method with all the options fields
            $this->init_form_fields();

            // Load the settings.
            $this->init_settings();
            $this->title              = $this->get_option('title');
            $this->description        = $this->get_option('description');
            $this->enabled            = $this->get_option('enabled');
            $this->merchant_address   = trim($this->get_option('merchant_address'));
            $this->node_url           = trim($this->get_option('node_url'));
            $this->exchange_rate      = $this->get_option('exchange_rate');
            $this->confirmation_level = $this->get_option('confirmation_level', 'included');

            // Action hook to save settings
            add_action('woocommerce_update_options_payment_gateways_' . $this->id, array($this, 'process_admin_options'));

            // Payment instructions on the order confirmation page
            add_action('woocommerce_thankyou_' . $this->id, array($this, 'thankyou_page_imoney_checkout'));
        }

        /**
         * Plugin options configuration in WooCommerce admin
         */
        public function init_form_fields() {
            $this->form_fields = array(
                'enabled' => array(
                    'title'       => __('Enable/Disable', 'imoney-payments'),
                    'label'       => __('Enable Internet Money (IMN) Gateway', 'imoney-payments'),
                    'type'        => 'checkbox',
                    'description' => '',
                    'default'     => 'yes'
                ),
                'title' => array(
                    'title'       => __('Title', 'imoney-payments'),
                    'type'        => 'text',
                    'description' => __('Payment method title visible to buyers during checkout.', 'imoney-payments'),
                    'default'     => __('Internet Money (IMN)', 'imoney-payments'),
                    'desc_tip'    => true,
                ),
                'description' => array(
                    'title'       => __('Description', 'imoney-payments'),
                    'type'        => 'textarea',
                    'description' => __('Payment method description shown during checkout.', 'imoney-payments'),
                    'default'     => __('Pay with Internet Money (IMN) from your own wallet. No intermediary fees.', 'imoney-payments'),
                ),
                'merchant_address' => array(
                    'title'       => __('Receiving Address (imn: or imntest:)', 'imoney-payments'),
                    'type'        => 'text',
                    'description' => __('Your own wallet address. Payments go straight to it.', 'imoney-payments'),
                    'default'     => '',
                    'desc_tip'    => true,
                ),
                'node_url' => array(
                    'title'       => __('Node URL', 'imoney-payments'),
                    'type'        => 'text',
                    'description' => __('Address of your Internet Money node, as reachable from this server. Only this server talks to it; customers do not.', 'imoney-payments'),
                    'default'     => 'http://127.0.0.1:18556',
                    'desc_tip'    => true,
                ),
                'exchange_rate' => array(
                    'title'       => __('Exchange Rate (IMN per 1 unit of store currency)', 'imoney-payments'),
                    'type'        => 'text',
                    'description' => __('Used to price each order in IMN at the moment it is placed.', 'imoney-payments'),
                    'default'     => '1.0',
                    'desc_tip'    => true,
                ),
                'confirmation_level' => array(
                    'title'       => __('Mark orders paid when the payment is', 'imoney-payments'),
                    'type'        => 'select',
                    'description' => __('Seen: the node has received it (under a second; it can still be replaced, so use only for small amounts). In a block: about 5 seconds. Final: buried under the number of blocks your node requires.', 'imoney-payments'),
                    'default'     => 'included',
                    'options'     => array(
                        'seen'     => __('Seen by the node', 'imoney-payments'),
                        'included' => __('In a block', 'imoney-payments'),
                        'final'    => __('Final', 'imoney-payments'),
                    ),
                ),
            );
        }

        /**
         * The gateway is offered at checkout only once it can actually take a payment.
         */
        public function is_available() {
            return parent::is_available() && !empty($this->merchant_address) && !empty($this->node_url);
        }

        /**
         * Prices the order in IMN, gives it an invoice number, and sends the buyer to the page
         * that shows how to pay. The order stays unpaid, and stock is not reduced, until the
         * node confirms the payment.
         */
        public function process_payment($order_id) {
            $order = wc_get_order($order_id);

            $rate = (float) $this->exchange_rate;
            if ($rate <= 0) {
                $rate = 1.0;
            }
            $atoms = imoney_imn_to_atoms(round(((float) $order->get_total()) * $rate, 8));
            // Unique per order and impossible to guess from the order number alone
            $invoice_id = 'wc-' . $order_id . '-' . substr(hash('sha256', $order->get_order_key()), 0, 10);

            $order->update_meta_data('_imoney_amount_atoms', (string) $atoms);
            $order->update_meta_data('_imoney_invoice_id', $invoice_id);
            $order->update_meta_data('_imoney_address', $this->merchant_address);
            $order->update_status('pending', __('Awaiting Internet Money (IMN) payment.', 'imoney-payments'));
            $order->save();

            // Clear cart
            WC()->cart->empty_cart();

            // Return thank you redirect
            return array(
                'result'   => 'success',
                'redirect' => $this->get_return_url($order)
            );
        }

        /**
         * Shows the payment request on the order confirmation page and watches for the payment.
         */
        public function thankyou_page_imoney_checkout($order_id) {
            $order = wc_get_order($order_id);
            if (!$order || $order->get_payment_method() !== $this->id) {
                return;
            }

            // Check with the node now, in case the payment has already arrived
            $state = imoney_check_order($order);
            if ($state['paid']) {
                echo '<p style="color: #0b7a4b; font-size: 16px; font-weight: bold;">' . esc_html__('Payment received. Thank you!', 'imoney-payments') . '</p>';
                return;
            }

            $atoms = (int) $order->get_meta('_imoney_amount_atoms');
            $invoice_id = $order->get_meta('_imoney_invoice_id');
            $address = $order->get_meta('_imoney_address');
            if ($atoms <= 0 || !$invoice_id || !$address) {
                return;
            }
            $amount_imn = imoney_atoms_to_imn($atoms);
            $payment_uri = $address . '?amount=' . $amount_imn . '&invoice=' . rawurlencode($invoice_id);

            $config = array(
                'uri'       => $payment_uri,
                'statusUrl' => add_query_arg('key', $order->get_order_key(), rest_url('imoney/v1/order/' . $order->get_id())),
                'texts'     => array(
                    'waiting'  => __('Waiting for your payment…', 'imoney-payments'),
                    'seen'     => __('Payment seen. Waiting for it to enter a block…', 'imoney-payments'),
                    'included' => __('Payment is in a block. Waiting for it to be buried deeper…', 'imoney-payments'),
                    'alert'    => __('Payment seen. The network is unsettled, so confirmation is taking longer.', 'imoney-payments'),
                    'paid'     => __('Payment received. Thank you!', 'imoney-payments'),
                    'copied'   => __('Copied!', 'imoney-payments'),
                ),
            );
            ?>
            <div id="imoney-checkout-container" style="margin: 20px 0; background: #f6f7ef; color: #0f2e23; border: 2px solid #0f2e23; max-width: 520px;">
                <div style="background: #0f2e23; color: #eef0e6; padding: 12px 16px; display: flex; align-items: center; gap: 10px; font-family: Georgia, 'Times New Roman', serif; font-size: 17px;">
                    <svg viewBox="0 0 512 512" width="22" height="22" aria-hidden="true"><rect width="512" height="512" fill="#eef0e6"/><rect x="86" y="128" width="56" height="256" fill="#0f2e23"/><path d="M182 384V232l60-104h56l-60 104v152z" fill="#0f2e23"/><path d="M310 384V232l60-104h56l-60 104v152z" fill="#0f2e23"/><circle cx="114" cy="94" r="16" fill="#0b7a4b"/></svg>
                    <?php esc_html_e('Pay with Internet Money', 'imoney-payments'); ?>
                </div>
                <div style="padding: 18px 20px 20px;">
                    <div style="border: 2px solid #0f2e23; padding: 3px; margin: 0 0 16px; max-width: 320px;">
                        <div style="border: 1px solid #0f2e23; padding: 12px 14px 10px;">
                            <p style="font-family: Georgia, 'Times New Roman', serif; font-size: 30px; font-weight: bold; line-height: 1.15; color: #0f2e23; margin: 0 0 4px;"><?php echo esc_html($amount_imn); ?> IMN</p>
                            <p style="font-family: ui-monospace, Consolas, Menlo, monospace; font-size: 12px; color: #3d6652; margin: 0;"><?php echo esc_html(sprintf(__('Invoice %s', 'imoney-payments'), $invoice_id)); ?></p>
                        </div>
                    </div>
                    <div id="imoney-qr" style="background: #fff; border: 1px solid #0f2e23; padding: 8px; width: 200px; height: 200px; box-sizing: border-box; margin-bottom: 14px;"></div>
                    <p style="font-size: 13px; color: #3d6652; margin: 0 0 6px;"><?php esc_html_e('Scan the code with your IMN wallet, or paste this payment request into it. It includes the invoice number, which is how we match your payment to this order.', 'imoney-payments'); ?></p>
                    <code id="imoney-uri" style="display: block; word-break: break-all; background: #e9edde; color: #0f2e23; border: 1px solid #a9b89a; padding: 8px 10px; font-size: 12px;"><?php echo esc_html($payment_uri); ?></code>
                    <button type="button" id="imoney-copy" style="margin-top: 8px; background: none; color: #0f2e23; border: 1px solid #0f2e23; border-radius: 0; padding: 5px 12px; cursor: pointer; font-size: 12px; letter-spacing: 0.08em; text-transform: uppercase;"><?php esc_html_e('Copy', 'imoney-payments'); ?></button>
                    <p id="imoney-status" role="status" style="margin: 16px 0 0; color: #3d6652; font-weight: bold;"><?php esc_html_e('Waiting for your payment…', 'imoney-payments'); ?></p>
                </div>
            </div>

            <script src="<?php echo esc_url(plugins_url('imoney.js', __FILE__)); ?>"></script>
            <script>
            (function () {
                var config = <?php echo wp_json_encode($config); ?>;
                var statusEl = document.getElementById('imoney-status');

                if (window.IMoney && IMoney.qrSvg) {
                    document.getElementById('imoney-qr').innerHTML = IMoney.qrSvg(config.uri);
                }
                document.getElementById('imoney-copy').addEventListener('click', function () {
                    var button = this;
                    var original = button.textContent;
                    navigator.clipboard.writeText(config.uri);
                    button.textContent = config.texts.copied;
                    setTimeout(function () { button.textContent = original; }, 2000);
                });

                // The server asks the node; this page only shows what the server reports
                function poll() {
                    fetch(config.statusUrl, { credentials: 'same-origin' })
                        .then(function (response) { return response.json(); })
                        .then(function (state) {
                            if (state.paid) {
                                statusEl.style.color = '#0b7a4b';
                                statusEl.textContent = config.texts.paid;
                                setTimeout(function () { window.location.reload(); }, 1500);
                                return;
                            }
                            if (state.seen) {
                                statusEl.style.color = '#94580a';
                                statusEl.textContent = state.network_alert
                                    ? config.texts.alert
                                    : (state.included ? config.texts.included : config.texts.seen);
                            }
                            setTimeout(poll, 3000);
                        })
                        .catch(function () { setTimeout(poll, 5000); });
                }
                poll();
            })();
            </script>
            <?php
        }
    }
}

// Register the WooCommerce gateway class
add_filter('woocommerce_payment_gateways', 'imoney_add_gateway_class');
function imoney_add_gateway_class($gateways) {
    $gateways[] = 'WC_Gateway_IMoney';
    return $gateways;
}

/**
 * Status endpoint polled by the order confirmation page. The caller must present the order
 * key, which only the buyer's confirmation link carries.
 */
add_action('rest_api_init', 'imoney_register_rest_routes');
function imoney_register_rest_routes() {
    register_rest_route('imoney/v1', '/order/(?P<id>\d+)', array(
        'methods'             => 'GET',
        'callback'            => 'imoney_rest_order_status',
        'permission_callback' => '__return_true',
        'args'                => array(
            'key' => array('required' => true, 'sanitize_callback' => 'sanitize_text_field'),
        ),
    ));
}

function imoney_rest_order_status($request) {
    if (!function_exists('wc_get_order')) {
        return new WP_Error('imoney_unavailable', 'WooCommerce is not active', array('status' => 503));
    }
    $order = wc_get_order((int) $request['id']);
    if (!$order || $order->get_payment_method() !== 'imoney' || !hash_equals($order->get_order_key(), (string) $request['key'])) {
        return new WP_Error('imoney_not_found', 'Order not found', array('status' => 404));
    }
    $state = imoney_check_order($order);
    return rest_ensure_response(array(
        'paid'          => (bool) $state['paid'],
        'seen'          => (bool) $state['seen'],
        'included'      => (bool) $state['included'],
        'network_alert' => (bool) $state['network_alert'],
    ));
}

/**
 * Background check, so an order is marked paid even if the buyer closes the page before the
 * payment confirms.
 */
add_filter('cron_schedules', 'imoney_cron_schedules');
function imoney_cron_schedules($schedules) {
    $schedules['imoney_five_minutes'] = array(
        'interval' => 300,
        'display'  => __('Every five minutes (Internet Money)', 'imoney-payments'),
    );
    return $schedules;
}

add_action(IMONEY_CRON_HOOK, 'imoney_check_pending_orders');
function imoney_check_pending_orders() {
    if (!function_exists('wc_get_orders')) {
        return;
    }
    $orders = wc_get_orders(array(
        'status'         => array('wc-pending', 'wc-on-hold'),
        'payment_method' => 'imoney',
        'limit'          => 50,
        'orderby'        => 'date',
        'order'          => 'DESC',
    ));
    foreach ($orders as $order) {
        imoney_check_order($order);
    }
}

register_activation_hook(__FILE__, 'imoney_activate');
function imoney_activate() {
    if (!wp_next_scheduled(IMONEY_CRON_HOOK)) {
        wp_schedule_event(time() + 60, 'imoney_five_minutes', IMONEY_CRON_HOOK);
    }
}

register_deactivation_hook(__FILE__, 'imoney_deactivate');
function imoney_deactivate() {
    wp_clear_scheduled_hook(IMONEY_CRON_HOOK);
}
