/**
 * Internet Money (IMN) Official Web & Node.js SDK
 * Ultra-fast, zero-custody, 5-second merchant payment checkout & BlockDAG API client.
 */

export const ATOMS_PER_IMN = 100_000_000n; // 1 IMN = 10^8 atoms (like satoshis)

export interface IMoneyClientConfig {
  nodeUrl?: string; // Default: 'http://127.0.0.1:18556'
  network?: 'mainnet' | 'testnet';
}

export interface NodeInfo {
  network: string;
  genesis_hash: string;
  total_blocks: number;
  virtual_selected_parent: string;
  virtual_blue_score: number;
  virtual_daa_score: number;
  tips: string[];
  current_bits: string;
  current_block_reward_imn: number;
  target_block_interval_sec: number;
  mining_address?: string;
  /** Name this as a payment's service address to give this node half of the fee. */
  service_address?: string;
  mempool_size: number;
}

export interface AddressBalance {
  address: string;
  balance_imn: number;
  balance_atoms: number;
}

export interface UtxoItem {
  transaction_id: string;
  index: number;
  value_atoms: number;
  value_imn: number;
}

export interface TxStatus {
  tx_id: string;
  status: 'pending' | 'confirmed' | 'not_found';
  block_hash?: string;
  daa_score?: number;
  /** 0 while pending; 1 once accepted, then one more per blue block added on top. */
  confirmations: number;
  inputs_count: number;
  outputs_count: number;
  total_output_atoms: number;
  total_output_imn: number;
}

export interface PaymentInvoiceOptions {
  merchantAddress: string;
  amountImn: number;
  orderId?: string;
  memo?: string;
  timeoutSeconds?: number;
  onSuccess?: (payment: { txId?: string; atoms: number; imn: number }) => void;
  onExpire?: () => void;
  onCancel?: () => void;
}

export class IMoneyClient {
  public readonly nodeUrl: string;
  public readonly network: 'mainnet' | 'testnet';

  constructor(config: IMoneyClientConfig = {}) {
    this.nodeUrl = (config.nodeUrl || 'http://127.0.0.1:18556').replace(/\/$/, '');
    this.network = config.network || 'testnet';
  }

  /**
   * Fetch current network status and blockDAG metrics.
   */
  async getInfo(): Promise<NodeInfo> {
    const res = await fetch(`${this.nodeUrl}/api/v1/info`);
    if (!res.ok) throw new Error(`Failed to fetch node info: ${res.statusText}`);
    return res.json();
  }

  /**
   * Retrieve current unspent balance for any IMN address.
   */
  async getBalance(address: string): Promise<AddressBalance> {
    const res = await fetch(`${this.nodeUrl}/api/v1/address/${encodeURIComponent(address)}/balance`);
    if (!res.ok) throw new Error(`Failed to get balance: ${res.statusText}`);
    return res.json();
  }

  /**
   * Retrieve spendable UTXOs for an address.
   */
  async getUtxos(address: string): Promise<UtxoItem[]> {
    const res = await fetch(`${this.nodeUrl}/api/v1/address/${encodeURIComponent(address)}/utxos`);
    if (!res.ok) throw new Error(`Failed to get UTXOs: ${res.statusText}`);
    return res.json();
  }

  /**
   * Query transaction confirmation status and receipt details.
   */
  async getTxStatus(txId: string): Promise<TxStatus> {
    const res = await fetch(`${this.nodeUrl}/api/v1/tx/${encodeURIComponent(txId)}`);
    if (!res.ok) throw new Error(`Failed to get transaction status: ${res.statusText}`);
    return res.json();
  }

  /**
   * Listen in real-time via WebSocket for incoming payments to an address.
   * Resolves immediately upon receiving new coins matching or exceeding expected atoms.
   */

  subscribeAddressPayments(
    address: string,
    onPayment: (data: { change_atoms: number; balance_atoms: number; balance_imn: number }) => void
  ): () => void {
    const wsUrl = this.nodeUrl.replace(/^http/, 'ws') + `/api/v1/ws/address/${encodeURIComponent(address)}`;
    let ws: any;
    let isClosed = false;

    if (typeof WebSocket !== 'undefined') {
      ws = new WebSocket(wsUrl);
    } else {
      // Node.js support fallback
      const NodeWs = require('ws');
      ws = new NodeWs(wsUrl);
    }

    ws.onmessage = (event: any) => {
      try {
        const payload = JSON.parse(event.data);
        if (payload.event === 'payment_received') {
          onPayment(payload);
        }
      } catch (err) {
        console.error('Error parsing IMN payment WS message:', err);
      }
    };

    return () => {
      isClosed = true;
      if (ws && ws.readyState === ws.OPEN) {
        ws.close();
      }
    };
  }

  /**
   * Launches an embeddable, responsive 1-click modal checkout widget.
   * Works on any website, CMS, or WebApp directly from JavaScript.
   */
  openCheckoutModal(options: PaymentInvoiceOptions) {
    if (typeof document === 'undefined') {
      throw new Error('openCheckoutModal can only be run in a browser environment');
    }

    const {
      merchantAddress,
      amountImn,
      orderId = 'ORD-' + Math.floor(100000 + Math.random() * 900000),
      memo = 'Store Purchase',
      timeoutSeconds = 600,
      onSuccess,
      onExpire,
      onCancel,
    } = options;

    const atomsRequired = Math.round(amountImn * 100_000_000);
    const paymentUri = `${merchantAddress}?amount=${amountImn}&label=${encodeURIComponent(memo)}`;

    // Create modal backdrop and overlay
    const overlay = document.createElement('div');
    overlay.id = 'imoney-modal-overlay';
    overlay.setAttribute(
      'style',
      'position: fixed; inset: 0; background: rgba(0,0,0,0.75); display: flex; align-items: center; justify-content: center; z-index: 999999; font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif;'
    );

    overlay.innerHTML = `
      <div style="background: #161b22; border: 1px solid #30363d; border-radius: 12px; width: 90%; max-width: 440px; padding: 24px; color: #f0f6fc; box-shadow: 0 10px 30px rgba(0,0,0,0.8); text-align: center; position: relative;">
        <button id="imoney-close-btn" style="position: absolute; top: 14px; right: 14px; background: none; border: none; color: #8b949e; font-size: 20px; cursor: pointer;">&times;</button>
        <div style="display: flex; align-items: center; justify-content: center; gap: 8px; margin-bottom: 8px;">
          <div style="width: 28px; height: 28px; background: #238636; border-radius: 50%; display: flex; align-items: center; justify-content: center; font-weight: bold; font-size: 16px;">⚡</div>
          <h2 style="margin: 0; font-size: 20px; color: #58a6ff;">Internet Money</h2>
        </div>
        <p style="margin: 4px 0 16px; color: #8b949e; font-size: 13px;">~5-Second BlockDAG Inclusion</p>
        
        <div style="background: #0d1117; border: 1px solid #21262d; border-radius: 8px; padding: 12px; margin-bottom: 16px;">
          <div style="font-size: 12px; color: #8b949e;">Amount to Send:</div>
          <div style="font-size: 26px; font-weight: bold; color: #39d353; margin: 4px 0;">${amountImn} IMN</div>
          <div style="font-size: 11px; color: #8b949e;">Order #${orderId} • Low Network Fee</div>
        </div>

        <!-- QR Code Placeholder -->
        <div style="background: white; border-radius: 8px; padding: 12px; display: inline-block; margin-bottom: 16px;">
          <img src="https://api.qrserver.com/v1/create-qr-code/?size=180x180&data=${encodeURIComponent(
            paymentUri
          )}" alt="Scan to pay" style="display: block; width: 180px; height: 180px;" />
        </div>

        <div style="margin-bottom: 16px; text-align: left;">
          <label style="font-size: 11px; color: #8b949e; display: block; margin-bottom: 4px;">Merchant Receiving Address:</label>
          <div style="background: #0d1117; border: 1px solid #30363d; border-radius: 6px; padding: 8px 10px; font-family: monospace; font-size: 11px; word-break: break-all; color: #79c0ff; display: flex; align-items: center; justify-content: space-between;">
            <span id="imoney-address-text">${merchantAddress}</span>
            <button id="imoney-copy-btn" style="background: #21262d; border: 1px solid #30363d; color: #c9d1d9; border-radius: 4px; padding: 2px 6px; font-size: 10px; cursor: pointer; margin-left: 6px;">Copy</button>
          </div>
        </div>

        <div id="imoney-status-box" style="display: flex; align-items: center; justify-content: center; gap: 8px; font-size: 13px; color: #db61a2; font-weight: 500;">
          <span style="display: inline-block; width: 10px; height: 10px; border-radius: 50%; background: #db61a2; animation: imoney-pulse 1.5s infinite;"></span>
          <span id="imoney-status-text">Listening on BlockDAG... (<span id="imoney-countdown">${timeoutSeconds}</span>s)</span>
        </div>
      </div>
      <style>
        @keyframes imoney-pulse {
          0% { transform: scale(0.9); opacity: 0.6; }
          50% { transform: scale(1.3); opacity: 1; }
          100% { transform: scale(0.9); opacity: 0.6; }
        }
      </style>
    `;

    document.body.appendChild(overlay);

    // Copy to clipboard
    const copyBtn = document.getElementById('imoney-copy-btn');
    if (copyBtn) {
      copyBtn.onclick = () => {
        navigator.clipboard.writeText(merchantAddress);
        copyBtn.innerText = 'Copied!';
        setTimeout(() => (copyBtn.innerText = 'Copy'), 2000);
      };
    }

    let remaining = timeoutSeconds;
    const countdownEl = document.getElementById('imoney-countdown');
    const timer = setInterval(() => {
      remaining -= 1;
      if (countdownEl) countdownEl.innerText = remaining.toString();
      if (remaining <= 0) {
        clearInterval(timer);
        cleanup();
        if (onExpire) onExpire();
      }
    }, 1000);

    const cleanup = () => {
      clearInterval(timer);
      unsubscribe();
      if (document.getElementById('imoney-modal-overlay')) {
        document.body.removeChild(overlay);
      }
    };

    // Close on cancel
    document.getElementById('imoney-close-btn')!.onclick = () => {
      cleanup();
      if (onCancel) onCancel();
    };

    // WebSocket live confirmation
    const unsubscribe = this.subscribeAddressPayments(merchantAddress, (data) => {
      if (data.change_atoms >= atomsRequired) {
        clearInterval(timer);
        const statusBox = document.getElementById('imoney-status-box');
        if (statusBox) {
          statusBox.innerHTML = `
            <div style="color: #39d353; font-weight: bold; font-size: 15px;">
              ✓ Payment received and included in the BlockDAG
            </div>
          `;
        }

        setTimeout(() => {
          cleanup();
          if (onSuccess) {
            onSuccess({
              atoms: data.change_atoms,
              imn: data.change_atoms / 100_000_000,
            });
          }
        }, 1500);
      }
    });
  }
}

// Global browser window export for simple CDN drop-in <script src="imoney.js">
if (typeof window !== 'undefined') {
  (window as any).IMoneyClient = IMoneyClient;
}
