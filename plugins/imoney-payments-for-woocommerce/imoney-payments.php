<?php
/**
 * Plugin Name: Internet Money (IMN) Payments for WooCommerce
 * Plugin URI: https://internetmoneynetwork.org
 * Description: Accept zero-fee, 5-second instant payments in Internet Money (IMN) cryptocurrency on your WooCommerce store. Direct to merchant address, 100% non-custodial.
 * Version: 1.0.0
 * Author: Internet Money Network Developers
 * Author URI: https://github.com/Internet-Money-Network
 * License: MIT
 * Text Domain: imoney-payments
 */

if (!defined('ABSPATH')) {
    exit; // Exit if accessed directly
}

// Ensure WooCommerce is active
add_action('plugins_loaded', 'imoney_payments_init_gateway_class');

function imoney_payments_init_gateway_class() {
    if (!class_exists('WC_Payment_Gateway')) {
        return;
    }

    class WC_Gateway_IMoney extends WC_Payment_Gateway {

        public function __construct() {
            $this->id                 = 'imoney';
            $this->icon               = apply_filters('woocommerce_imoney_icon', '');
            $this->has_fields         = false;
            $this->method_title       = __('Internet Money (IMN)', 'imoney-payments');
            $this->method_description = __('Accept instant, zero-fee IMN payments directly to your wallet via the high-speed BlockDAG.', 'imoney-payments');

            $this->supports = array(
                'products',
            );

            // Method with all the options fields
            $this->init_form_fields();

            // Load the settings.
            $this->init_settings();
            $this->title            = $this->get_option('title');
            $this->description      = $this->get_option('description');
            $this->enabled          = $this->get_option('enabled');
            $this->merchant_address = $this->get_option('merchant_address');
            $this->node_url         = $this->get_option('node_url');
            $this->exchange_rate    = $this->get_option('exchange_rate');

            // Action hook to save settings
            add_action('woocommerce_update_options_payment_gateways_' . $this->id, array($this, 'process_admin_options'));

            // Thank you page custom hook for interactive live modal checkout
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
                    'default'     => __('Internet Money (IMN) - 5s Instant Pay', 'imoney-payments'),
                    'desc_tip'    => true,
                ),
                'description' => array(
                    'title'       => __('Description', 'imoney-payments'),
                    'type'        => 'textarea',
                    'description' => __('Payment method description shown during checkout.', 'imoney-payments'),
                    'default'     => __('Pay instantly with zero intermediary fees using Internet Money (IMN) BlockDAG cryptocurrency.', 'imoney-payments'),
                ),
                'merchant_address' => array(
                    'title'       => __('Merchant Receiving Address (imn: or imntest:)', 'imoney-payments'),
                    'type'        => 'text',
                    'description' => __('Your non-custodial Bech32 wallet address. All buyer payments settle directly into your wallet.', 'imoney-payments'),
                    'default'     => 'imntest:qpm2qsznhks23z7629mms6s4cwef74vcwvy22gd6aedxrr66r20',
                    'desc_tip'    => true,
                ),
                'node_url' => array(
                    'title'       => __('Node API / RPC URL', 'imoney-payments'),
                    'type'        => 'text',
                    'description' => __('Public or local node endpoint for real-time WebSocket payment detection.', 'imoney-payments'),
                    'default'     => 'http://127.0.0.1:18556',
                    'desc_tip'    => true,
                ),
                'exchange_rate' => array(
                    'title'       => __('Exchange Rate (1 USD = ? IMN)', 'imoney-payments'),
                    'type'        => 'text',
                    'description' => __('Fixed conversion rate or testnet peg factor for pricing your goods in IMN.', 'imoney-payments'),
                    'default'     => '1.0',
                    'desc_tip'    => true,
                )
            );
        }

        /**
         * Process Order: Marks order on-hold awaiting BlockDAG payment, redirects to thankyou page.
         */
        public function process_payment($order_id) {
            $order = wc_get_order($order_id);

            // Mark as on-hold (awaiting IMN transaction)
            $order->update_status('on-hold', __('Awaiting Internet Money (IMN) BlockDAG settlement.', 'imoney-payments'));

            // Reduce stock levels
            wc_reduce_stock_levels($order_id);

            // Clear cart
            WC()->cart->empty_cart();

            // Return thank you redirect
            return array(
                'result'   => 'success',
                'redirect' => $this->get_return_url($order)
            );
        }

        /**
         * Render the live 1-click checkout modal on the order confirmation screen
         */
        public function thankyou_page_imoney_checkout($order_id) {
            $order = wc_get_order($order_id);
            if (!$order || $order->get_payment_method() !== $this->id) {
                return;
            }

            if ($order->is_paid()) {
                echo '<p style="color: #238636; font-size: 16px; font-weight: bold;">✓ This order has been fully paid and confirmed on the Internet Money BlockDAG.</p>';
                return;
            }

            $total = (float)$order->get_total();
            $rate = (float)($this->exchange_rate ?: 1.0);
            $imn_amount = round($total * $rate, 4);

            $merchant_addr = esc_attr($this->merchant_address);
            $node_url = esc_attr($this->node_url);

            ?>
            <div id="imoney-checkout-container" style="margin: 20px 0; padding: 20px; background: #0d1117; color: #f0f6fc; border-radius: 8px; border: 1px solid #30363d;">
                <h3 style="color: #58a6ff; margin-top: 0;">⚡ Internet Money Instant Checkout</h3>
                <p>Complete your payment using any IMN wallet. Confirms in ~5 seconds with 0 network fees.</p>
                <button id="btn-trigger-imoney" style="background: #238636; color: white; border: none; padding: 12px 24px; font-size: 16px; border-radius: 6px; cursor: pointer; font-weight: bold;">
                    Pay <?php echo esc_html($imn_amount); ?> IMN Now
                </button>
            </div>

            <!-- Embed @imoney/sdk bundled client -->
            <script src="<?php echo esc_url(plugins_url('imoney.js', __FILE__)); ?>"></script>
            <script>
                document.getElementById('btn-trigger-imoney').addEventListener('click', function() {
                    if (typeof IMoneyClient === 'undefined') {
                        alert('Internet Money SDK is loading, please try again in a moment.');
                        return;
                    }

                    var client = new IMoneyClient({
                        nodeUrl: '<?php echo $node_url; ?>'
                    });

                    client.openCheckoutModal({
                        merchantAddress: '<?php echo $merchant_addr; ?>',
                        amountIm: <?php echo $imn_amount; ?>,
                        orderId: '<?php echo esc_attr($order_id); ?>',
                        memo: 'Order #<?php echo esc_attr($order_id); ?>',
                        onSuccess: function(payment) {
                            // Update UI and reload
                            var container = document.getElementById('imoney-checkout-container');
                            if (container) {
                                container.innerHTML = '<div style="background: #1f6feb; padding: 16px; border-radius: 6px; color: white; font-weight: bold;">Payment Verified! Transaction confirmed on BlockDAG.</div>';
                            }
                            setTimeout(function() {
                                window.location.reload();
                            }, 2000);
                        }
                    });
                });
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
