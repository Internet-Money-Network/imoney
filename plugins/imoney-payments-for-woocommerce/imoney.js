(function(global, factory) {
  if (typeof exports === 'object' && typeof module !== 'undefined') {
    factory(exports);
  } else if (typeof define === 'function' && define.amd) {
    define(['exports'], factory);
  } else {
    global = typeof globalThis !== 'undefined' ? globalThis : global || self;
    factory(global.IMoney = {});
  }
})(this, function(exports) {
  'use strict';

  var ATOMS_PER_IMN = 100000000;

  function IMoneyClient(config) {
    config = config || {};
    this.nodeUrl = (config.nodeUrl || 'http://127.0.0.1:18556').replace(/\/$/, '');
    this.network = config.network || 'testnet';
  }

  IMoneyClient.prototype.getInfo = async function() {
    var res = await fetch(this.nodeUrl + '/api/v1/info');
    if (!res.ok) throw new Error('Failed to fetch node info: ' + res.statusText);
    return res.json();
  };

  IMoneyClient.prototype.getBalance = async function(address) {
    var res = await fetch(this.nodeUrl + '/api/v1/address/' + encodeURIComponent(address) + '/balance');
    if (!res.ok) throw new Error('Failed to get balance: ' + res.statusText);
    return res.json();
  };

  IMoneyClient.prototype.getUtxos = async function(address) {
    var res = await fetch(this.nodeUrl + '/api/v1/address/' + encodeURIComponent(address) + '/utxos');
    if (!res.ok) throw new Error('Failed to get UTXOs: ' + res.statusText);
    return res.json();
  };

  IMoneyClient.prototype.getTxStatus = async function(txId) {
    var res = await fetch(this.nodeUrl + '/api/v1/tx/' + encodeURIComponent(txId));
    if (!res.ok) throw new Error('Failed to get transaction status: ' + res.statusText);
    return res.json();
  };

  IMoneyClient.prototype.subscribeAddressPayments = function(address, onPayment) {
    var wsUrl = this.nodeUrl.replace(/^http/, 'ws') + '/api/v1/ws/address/' + encodeURIComponent(address);
    var ws = new WebSocket(wsUrl);

    ws.onmessage = function(event) {
      try {
        var payload = JSON.parse(event.data);
        if (payload.event === 'payment_received') {
          onPayment(payload);
        }
      } catch (err) {
        console.error('Error parsing IMN payment WS message:', err);
      }
    };

    return function() {
      if (ws && ws.readyState === WebSocket.OPEN) {
        ws.close();
      }
    };
  };

  IMoneyClient.prototype.openCheckoutModal = function(options) {
    if (typeof document === 'undefined') {
      throw new Error('openCheckoutModal can only be run in a browser environment');
    }

    var merchantAddress = options.merchantAddress;
    var amountImn = options.amountImn;
    var orderId = options.orderId || ('ORD-' + Math.floor(100000 + Math.random() * 900000));
    var memo = options.memo || 'Store Purchase';
    var timeoutSeconds = options.timeoutSeconds || 600;
    var onSuccess = options.onSuccess;
    var onExpire = options.onExpire;
    var onCancel = options.onCancel;

    var atomsRequired = Math.round(amountImn * ATOMS_PER_IMN);
    var paymentUri = merchantAddress + '?amount=' + amountImn + '&label=' + encodeURIComponent(memo);

    var overlay = document.createElement('div');
    overlay.id = 'imoney-modal-overlay';
    overlay.setAttribute(
      'style',
      'position: fixed; inset: 0; background: rgba(0,0,0,0.75); display: flex; align-items: center; justify-content: center; z-index: 999999; font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif;'
    );

    overlay.innerHTML =
      '<div style="background: #161b22; border: 1px solid #30363d; border-radius: 12px; width: 90%; max-width: 440px; padding: 24px; color: #f0f6fc; box-shadow: 0 10px 30px rgba(0,0,0,0.8); text-align: center; position: relative;">' +
      '  <button id="imoney-close-btn" style="position: absolute; top: 14px; right: 14px; background: none; border: none; color: #8b949e; font-size: 20px; cursor: pointer;">&times;</button>' +
      '  <div style="display: flex; align-items: center; justify-content: center; gap: 8px; margin-bottom: 8px;">' +
      '    <div style="width: 28px; height: 28px; background: #238636; border-radius: 50%; display: flex; align-items: center; justify-content: center; font-weight: bold; font-size: 16px;">⚡</div>' +
      '    <h2 style="margin: 0; font-size: 20px; color: #58a6ff;">Internet Money</h2>' +
      '  </div>' +
      '  <p style="margin: 4px 0 16px; color: #8b949e; font-size: 13px;">~5-Second BlockDAG Inclusion</p>' +
      '  <div style="background: #0d1117; border: 1px solid #21262d; border-radius: 8px; padding: 12px; margin-bottom: 16px;">' +
      '    <div style="font-size: 12px; color: #8b949e;">Amount to Send:</div>' +
      '    <div style="font-size: 26px; font-weight: bold; color: #39d353; margin: 4px 0;">' + amountImn + ' IMN</div>' +
      '    <div style="font-size: 11px; color: #8b949e;">Order #' + orderId + ' • Low Network Fee</div>' +
      '  </div>' +
      '  <div style="background: white; border-radius: 8px; padding: 12px; display: inline-block; margin-bottom: 16px;">' +
      '    <img src="https://api.qrserver.com/v1/create-qr-code/?size=180x180&data=' + encodeURIComponent(paymentUri) + '" alt="Scan to pay" style="display: block; width: 180px; height: 180px;" />' +
      '  </div>' +
      '  <div style="margin-bottom: 16px; text-align: left;">' +
      '    <label style="font-size: 11px; color: #8b949e; display: block; margin-bottom: 4px;">Merchant Receiving Address:</label>' +
      '    <div style="background: #0d1117; border: 1px solid #30363d; border-radius: 6px; padding: 8px 10px; font-family: monospace; font-size: 11px; word-break: break-all; color: #79c0ff; display: flex; align-items: center; justify-content: space-between;">' +
      '      <span id="imoney-address-text">' + merchantAddress + '</span>' +
      '      <button id="imoney-copy-btn" style="background: #21262d; border: 1px solid #30363d; color: #c9d1d9; border-radius: 4px; padding: 2px 6px; font-size: 10px; cursor: pointer; margin-left: 6px;">Copy</button>' +
      '    </div>' +
      '  </div>' +
      '  <div id="imoney-status-box" style="display: flex; align-items: center; justify-content: center; gap: 8px; font-size: 13px; color: #db61a2; font-weight: 500;">' +
      '    <span style="display: inline-block; width: 10px; height: 10px; border-radius: 50%; background: #db61a2; animation: imoney-pulse 1.5s infinite;"></span>' +
      '    <span id="imoney-status-text">Listening on BlockDAG... (<span id="imoney-countdown">' + timeoutSeconds + '</span>s)</span>' +
      '  </div>' +
      '</div>' +
      '<style>' +
      '  @keyframes imoney-pulse {' +
      '    0% { transform: scale(0.9); opacity: 0.6; }' +
      '    50% { transform: scale(1.3); opacity: 1; }' +
      '    100% { transform: scale(0.9); opacity: 0.6; }' +
      '  }' +
      '</style>';

    document.body.appendChild(overlay);

    var copyBtn = document.getElementById('imoney-copy-btn');
    if (copyBtn) {
      copyBtn.onclick = function() {
        navigator.clipboard.writeText(merchantAddress);
        copyBtn.innerText = 'Copied!';
        setTimeout(function() { copyBtn.innerText = 'Copy'; }, 2000);
      };
    }

    var remaining = timeoutSeconds;
    var countdownEl = document.getElementById('imoney-countdown');
    var timer = setInterval(function() {
      remaining -= 1;
      if (countdownEl) countdownEl.innerText = remaining.toString();
      if (remaining <= 0) {
        clearInterval(timer);
        cleanup();
        if (onExpire) onExpire();
      }
    }, 1000);

    var cleanup = function() {
      clearInterval(timer);
      unsubscribe();
      if (document.getElementById('imoney-modal-overlay')) {
        document.body.removeChild(overlay);
      }
    };

    document.getElementById('imoney-close-btn').onclick = function() {
      cleanup();
      if (onCancel) onCancel();
    };

    var unsubscribe = this.subscribeAddressPayments(merchantAddress, function(data) {
      if (data.change_atoms >= atomsRequired) {
        clearInterval(timer);
        var statusBox = document.getElementById('imoney-status-box');
        if (statusBox) {
          statusBox.innerHTML = '<div style="color: #39d353; font-weight: bold; font-size: 15px;">✓ Payment received and included in the BlockDAG</div>';
        }

        setTimeout(function() {
          cleanup();
          if (onSuccess) {
            onSuccess({
              atoms: data.change_atoms,
              imn: data.change_atoms / ATOMS_PER_IMN
            });
          }
        }, 1500);
      }
    });
  };

  exports.IMoneyClient = IMoneyClient;
  exports.ATOMS_PER_IMN = ATOMS_PER_IMN;

  if (typeof window !== 'undefined') {
    window.IMoneyClient = IMoneyClient;
  }
});
