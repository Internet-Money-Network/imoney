/*
 * Registers Internet Money with WooCommerce's block-based checkout, so it is offered as a
 * payment method there. Paying happens afterwards, on the order confirmation page.
 */
(function () {
    if (!window.wc || !window.wc.wcBlocksRegistry || !window.wc.wcSettings || !window.wp || !window.wp.element) {
        return;
    }
    var settings = window.wc.wcSettings.getSetting('imoney_data', {});
    var el = window.wp.element.createElement;
    var decode = window.wp.htmlEntities ? window.wp.htmlEntities.decodeEntities : function (text) { return text; };
    var title = decode(settings.title || 'Internet Money (IMN)');
    var description = decode(settings.description || '');

    function Content() {
        return el('div', null, description);
    }

    window.wc.wcBlocksRegistry.registerPaymentMethod({
        name: 'imoney',
        label: el('span', null, title),
        ariaLabel: title,
        content: el(Content),
        edit: el(Content),
        canMakePayment: function () { return true; },
        supports: { features: settings.supports || ['products'] }
    });
})();
