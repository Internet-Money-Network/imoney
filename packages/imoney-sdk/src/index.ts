/**
 * Internet Money (IMN) SDK for the browser and Node.js.
 *
 * Talks to an Internet Money node, creates invoices, and tells you when an invoice has been
 * paid and how settled the payment is. It never handles private keys.
 */

import qrcode from 'qrcode-generator';

/** 1 IMN = 10^8 atoms. */
export const ATOMS_PER_IMN = 100_000_000;

/**
 * How settled a payment is:
 * - `seen`: the node has received it (under a second). It can still be replaced.
 * - `included`: it is in a block (about 5 seconds).
 * - `final`: it is buried under the number of blocks the node requires, and the network is calm.
 */
export type PaymentLevel = 'seen' | 'included' | 'final';

export interface IMoneyClientConfig {
  /** Address of the node to talk to. Default: 'http://127.0.0.1:18556' */
  nodeUrl?: string;
  network?: 'mainnet' | 'testnet';
}

/** One line of an address's history: the net effect of one transaction on the address. */
export interface HistoryItem {
  /** The transaction. For a mining reward, the id of the coin the reward created. */
  tx_id: string;
  kind: 'received' | 'sent' | 'reward';
  received_atoms: number;
  sent_atoms: number;
  /** received_atoms - sent_atoms */
  net_atoms: number;
  net_imn: number;
  confirmations: number;
  /** Time of the block carrying the transaction, as its miner reported it. */
  timestamp_ms: number;
}

export interface AddressHistory {
  address: string;
  /** Newest first. */
  items: HistoryItem[];
  /** Pass as `before` to read the next, older page. Absent on the last page. */
  next?: string | null;
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
  finality_depth: number;
  finality_conflict: boolean;
  last_reorg_depth?: number;
  last_reorg_at_ms?: number;
  /** True while the network looks unsettled; nothing is reported as final while it is set. */
  network_alert: boolean;
  final_confirmations: number;
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
  /** False for a block reward that has not matured yet. */
  spendable: boolean;
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

export interface InvoicePayment {
  tx_id: string;
  amount_atoms: number;
  confirmations: number;
  level: PaymentLevel;
}

/** What has been paid towards an invoice. `seen_atoms >= included_atoms >= final_atoms`. */
export interface InvoiceStatus {
  invoice_id: string;
  address: string;
  payments: InvoicePayment[];
  seen_atoms: number;
  included_atoms: number;
  final_atoms: number;
  final_confirmations: number;
  network_alert: boolean;
}

/** A request for one payment. Create it with `IMoneyClient.createInvoice`. */
export interface Invoice {
  invoiceId: string;
  address: string;
  amountAtoms: number;
  amountImn: string;
  /** What a wallet scans or pastes: the address with the amount and invoice ID attached. */
  uri: string;
}

export interface WaitOptions {
  /** How settled the payment must be before the promise resolves. Default: 'included'. */
  level?: PaymentLevel;
  /** Give up after this long. Default: 10 minutes. */
  timeoutMs?: number;
  /** How often to ask the node when no push arrives. Default: 2 seconds. */
  pollMs?: number;
  /** Called with every status read, so a page can show progress. */
  onProgress?: (status: InvoiceStatus) => void;
  /** Abort the wait from outside. */
  signal?: AbortSignal;
}

export interface CheckoutOptions {
  merchantAddress: string;
  amountImn: number | string;
  /** Your order reference. Used as the invoice ID, so it must be unique per order. */
  invoiceId?: string;
  /** Alias of `invoiceId`, kept for older integrations. */
  orderId?: string;
  memo?: string;
  timeoutSeconds?: number;
  /** How settled the payment must be before `onSuccess` fires. Default: 'included'. */
  level?: PaymentLevel;
  onSeen?: (status: InvoiceStatus) => void;
  onSuccess?: (payment: { invoiceId: string; txIds: string[]; atoms: number; imn: number; level: PaymentLevel }) => void;
  onExpire?: () => void;
  onCancel?: () => void;
}

const INVOICE_ID_PATTERN = /^[A-Za-z0-9._-]{1,64}$/;

/** An invoice ID is 1 to 64 characters from `A-Z a-z 0-9 - _ .` */
export function isValidInvoiceId(id: string): boolean {
  return INVOICE_ID_PATTERN.test(id);
}

/** Converts an IMN amount to atoms exactly, without floating-point rounding. */
export function imnToAtoms(amount: number | string): number {
  const text = typeof amount === 'number' ? amount.toFixed(8) : amount.trim();
  const match = /^(\d+)(?:\.(\d{1,8}))?$/.exec(text);
  if (!match) throw new Error(`Invalid IMN amount: ${amount}`);
  const atoms = BigInt(match[1]) * BigInt(ATOMS_PER_IMN) + BigInt((match[2] || '').padEnd(8, '0'));
  if (atoms > BigInt(Number.MAX_SAFE_INTEGER)) throw new Error('Amount is too large');
  return Number(atoms);
}

/** Formats atoms as an IMN amount with trailing zeros removed. */
export function atomsToImn(atoms: number): string {
  const whole = Math.floor(atoms / ATOMS_PER_IMN);
  const fraction = String(atoms % ATOMS_PER_IMN).padStart(8, '0').replace(/0+$/, '');
  return fraction ? `${whole}.${fraction}` : String(whole);
}

/** A random invoice ID. Prefer your own order number when you have one. */
export function newInvoiceId(prefix = 'inv'): string {
  const bytes = new Uint8Array(8);
  globalThis.crypto.getRandomValues(bytes);
  return `${prefix}-${Array.from(bytes, (b) => b.toString(16).padStart(2, '0')).join('')}`;
}

/** Builds the text a wallet scans: `imn:q...?amount=12.5&invoice=INV-1042`. */
export function paymentUri(address: string, amountImn: string, invoiceId?: string): string {
  const query = [`amount=${amountImn}`];
  if (invoiceId) query.push(`invoice=${encodeURIComponent(invoiceId)}`);
  return `${address}?${query.join('&')}`;
}

/** Reads a payment URI back into its parts. A bare address is accepted too. */
export function parsePaymentUri(uri: string): { address: string; amountImn?: string; invoiceId?: string } {
  const [address, query = ''] = uri.trim().split('?');
  const params = new URLSearchParams(query);
  const amountImn = params.get('amount') || undefined;
  const invoiceId = params.get('invoice') || undefined;
  if (amountImn !== undefined) imnToAtoms(amountImn);
  if (invoiceId !== undefined && !isValidInvoiceId(invoiceId)) throw new Error('Invalid invoice ID in payment URI');
  return { address, amountImn, invoiceId };
}

/** Renders text as a QR code, returned as an SVG string. Generated locally; nothing is sent anywhere. */
export function qrSvg(text: string, cellSize = 4, margin = 2): string {
  const qr = qrcode(0, 'M');
  qr.addData(text);
  qr.make();
  return qr.createSvgTag({ cellSize, margin, scalable: true });
}

function escapeHtml(text: string): string {
  return text.replace(/[&<>"']/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' })[c] as string);
}

function atomsAt(status: InvoiceStatus, level: PaymentLevel): number {
  return level === 'seen' ? status.seen_atoms : level === 'included' ? status.included_atoms : status.final_atoms;
}

export class IMoneyClient {
  public readonly nodeUrl: string;
  public readonly network: 'mainnet' | 'testnet';

  constructor(config: IMoneyClientConfig = {}) {
    this.nodeUrl = (config.nodeUrl || 'http://127.0.0.1:18556').replace(/\/$/, '');
    this.network = config.network || 'testnet';
  }

  private async get<T>(path: string, what: string): Promise<T> {
    const res = await fetch(`${this.nodeUrl}${path}`);
    if (!res.ok) throw new Error(`Failed to ${what}: ${res.status} ${res.statusText}`);
    return res.json() as Promise<T>;
  }

  /** Fetch current network status and blockDAG metrics. */
  getInfo(): Promise<NodeInfo> {
    return this.get('/api/v1/info', 'fetch node info');
  }

  /** Retrieve the balance of any IMN address. */
  getBalance(address: string): Promise<AddressBalance> {
    return this.get(`/api/v1/address/${encodeURIComponent(address)}/balance`, 'get balance');
  }

  /** Retrieve the unspent outputs of an address. */
  getUtxos(address: string): Promise<UtxoItem[]> {
    return this.get(`/api/v1/address/${encodeURIComponent(address)}/utxos`, 'get UTXOs');
  }

  /**
   * What an address received and sent, newest first. Only transactions accepted into the
   * ledger are listed. Pass the previous page's `next` as `before` to continue.
   */
  getHistory(address: string, options: { limit?: number; before?: string } = {}): Promise<AddressHistory> {
    const query = new URLSearchParams();
    if (options.limit) query.set('limit', String(options.limit));
    if (options.before) query.set('before', options.before);
    const suffix = query.toString() ? `?${query}` : '';
    return this.get(`/api/v1/address/${encodeURIComponent(address)}/history${suffix}`, 'get history');
  }

  /** Query a transaction's status and confirmations. */
  getTxStatus(txId: string): Promise<TxStatus> {
    return this.get(`/api/v1/tx/${encodeURIComponent(txId)}`, 'get transaction status');
  }

  /** Submit a signed transaction (as produced by the wallet's signing module). */
  async broadcastTransaction(transaction: unknown): Promise<{ success: boolean; tx_id?: string; error?: string }> {
    const res = await fetch(`${this.nodeUrl}/api/v1/tx/broadcast`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ transaction }),
    });
    return res.json();
  }

  /**
   * Describes a payment you expect. Nothing is sent to the node: an invoice exists once a
   * payment naming its ID arrives. Use an ID that is unique per order.
   */
  createInvoice(options: { address: string; amountImn: number | string; invoiceId?: string }): Invoice {
    const invoiceId = options.invoiceId ?? newInvoiceId();
    if (!isValidInvoiceId(invoiceId)) {
      throw new Error('Invoice ID must be 1-64 characters of A-Z a-z 0-9 - _ .');
    }
    const amountAtoms = imnToAtoms(options.amountImn);
    if (amountAtoms <= 0) throw new Error('Invoice amount must be positive');
    const amountImn = atomsToImn(amountAtoms);
    return { invoiceId, address: options.address, amountAtoms, amountImn, uri: paymentUri(options.address, amountImn, invoiceId) };
  }

  /** What has been paid towards an invoice so far. */
  getInvoice(invoiceId: string, address: string): Promise<InvoiceStatus> {
    return this.get(
      `/api/v1/invoice/${encodeURIComponent(invoiceId)}?address=${encodeURIComponent(address)}`,
      'get invoice status'
    );
  }

  /**
   * Resolves once the invoice has been paid in full at the requested level, and rejects on
   * timeout or abort. The node's own records decide; a push from the node only makes the next
   * check happen sooner.
   */
  waitForPayment(invoice: Invoice, options: WaitOptions = {}): Promise<InvoiceStatus> {
    const { level = 'included', timeoutMs = 600_000, pollMs = 2_000, onProgress, signal } = options;

    return new Promise((resolve, reject) => {
      let finished = false;
      let timer: ReturnType<typeof setTimeout> | undefined;
      let checking = false;

      const finish = (settle: () => void) => {
        if (finished) return;
        finished = true;
        if (timer) clearTimeout(timer);
        clearTimeout(deadline);
        unsubscribe();
        signal?.removeEventListener('abort', onAbort);
        settle();
      };
      const onAbort = () => finish(() => reject(new Error('Payment wait was cancelled')));
      const deadline = setTimeout(() => finish(() => reject(new Error('Timed out waiting for payment'))), timeoutMs);

      const check = async () => {
        if (finished || checking) return;
        checking = true;
        try {
          const status = await this.getInvoice(invoice.invoiceId, invoice.address);
          if (finished) return;
          onProgress?.(status);
          if (atomsAt(status, level) >= invoice.amountAtoms) {
            finish(() => resolve(status));
            return;
          }
        } catch {
          // The node may be briefly unreachable; keep trying until the deadline
        } finally {
          checking = false;
        }
        if (!finished) {
          if (timer) clearTimeout(timer);
          timer = setTimeout(check, pollMs);
        }
      };

      const unsubscribe = this.subscribeAddressPayments(invoice.address, () => void check());
      if (signal?.aborted) {
        onAbort();
        return;
      }
      signal?.addEventListener('abort', onAbort);
      void check();
    });
  }

  /**
   * Listens for activity on an address over WebSocket. `onEvent` receives `payment_seen` when a
   * payment reaches the node and `payment_received` when the balance changes. Returns a function
   * that stops listening. Where WebSocket is unavailable it does nothing; polling still works.
   */
  subscribeAddressPayments(address: string, onEvent: (data: any) => void): () => void {
    if (typeof WebSocket === 'undefined') return () => {};
    const wsUrl = this.nodeUrl.replace(/^http/, 'ws') + `/api/v1/ws/address/${encodeURIComponent(address)}`;
    let ws: WebSocket;
    try {
      ws = new WebSocket(wsUrl);
    } catch {
      return () => {};
    }
    ws.onmessage = (event: MessageEvent) => {
      try {
        const payload = JSON.parse(String(event.data));
        if (payload.event === 'payment_seen' || payload.event === 'payment_received') onEvent(payload);
      } catch {
        // Ignore anything that is not one of the node's JSON messages
      }
    };
    ws.onerror = () => {};
    return () => {
      try {
        ws.close();
      } catch {
        // Already closed
      }
    };
  }

  /**
   * Opens a checkout window for one invoice: amount, QR code, address and live status.
   * Browser only. The payment is matched by invoice ID and amount, never by balance alone.
   */
  openCheckoutModal(options: CheckoutOptions): { invoice: Invoice; close: () => void } {
    if (typeof document === 'undefined') {
      throw new Error('openCheckoutModal can only be run in a browser environment');
    }
    const { merchantAddress, timeoutSeconds = 600, level = 'included', memo, onSeen, onSuccess, onExpire, onCancel } = options;
    const invoice = this.createInvoice({
      address: merchantAddress,
      amountImn: options.amountImn,
      invoiceId: options.invoiceId ?? options.orderId,
    });

    const overlay = document.createElement('div');
    overlay.id = 'imoney-modal-overlay';
    overlay.setAttribute(
      'style',
      'position: fixed; inset: 0; background: rgba(8, 20, 15, 0.72); display: flex; align-items: center; justify-content: center; z-index: 999999; font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif;'
    );
    // Note paper and engraver's green, with fonts every device has: this window opens inside
    // other people's shops, so it loads nothing from elsewhere. Single quotes only: these go
    // inside double-quoted style attributes.
    const serif = "Georgia, 'Iowan Old Style', 'Times New Roman', serif";
    const mono = "ui-monospace, 'Cascadia Mono', Consolas, Menlo, monospace";
    const caps = `font-family: ${mono}; font-size: 10.5px; letter-spacing: 0.12em; text-transform: uppercase; color: #3d6652;`;
    overlay.innerHTML = `
      <div style="background: #f6f7ef; border: 2px solid #0f2e23; width: 92%; max-width: 420px; max-height: 94vh; overflow-y: auto; color: #0f2e23; box-shadow: 0 18px 50px rgba(0,0,0,0.45); text-align: center; position: relative;">
        <div style="background: #0f2e23; color: #eef0e6; padding: 12px 16px; display: flex; align-items: center; justify-content: space-between; gap: 12px;">
          <span style="display: flex; align-items: center; gap: 10px; font-family: ${serif}; font-size: 17px;">
            <svg viewBox="0 0 512 512" width="22" height="22" aria-hidden="true"><rect width="512" height="512" fill="#eef0e6"/><rect x="86" y="128" width="56" height="256" fill="#0f2e23"/><path d="M182 384V232l60-104h56l-60 104v152z" fill="#0f2e23"/><path d="M310 384V232l60-104h56l-60 104v152z" fill="#0f2e23"/><circle cx="114" cy="94" r="16" fill="#0b7a4b"/></svg>
            Pay with Internet Money
          </span>
          <button data-imoney="close" aria-label="Close" style="background: none; border: 1px solid #b9c9a4; color: #eef0e6; font-family: ${mono}; font-size: 10.5px; letter-spacing: 0.12em; padding: 4px 8px; cursor: pointer;">CLOSE</button>
        </div>
        <div style="padding: 18px 22px 20px;">
          <p style="margin: 0 0 14px; color: #3d6652; font-size: 13.5px;">${escapeHtml(memo || 'Scan with your IMN wallet, or copy the payment request')}</p>

          <div style="border: 2px solid #0f2e23; padding: 3px; margin-bottom: 16px;">
            <div style="border: 1px solid #0f2e23; padding: 12px 10px 10px;">
              <div style="${caps}">Amount</div>
              <div style="font-family: ${serif}; font-size: 32px; font-weight: bold; line-height: 1.15; margin: 4px 0;">${escapeHtml(invoice.amountImn)} IMN</div>
              <div style="font-family: ${mono}; font-size: 11.5px; color: #3d6652;">Invoice ${escapeHtml(invoice.invoiceId)}</div>
            </div>
          </div>

          <div style="background: #ffffff; border: 1px solid #0f2e23; padding: 8px; display: inline-block; margin-bottom: 16px; width: 200px; height: 200px; box-sizing: border-box;">${qrSvg(invoice.uri)}</div>

          <div style="margin-bottom: 16px; text-align: left;">
            <label style="${caps} display: block; margin-bottom: 5px;">Payment request (includes the invoice number)</label>
            <div style="background: #e9edde; border: 1px solid #a9b89a; padding: 8px 10px; font-family: ${mono}; font-size: 11.5px; word-break: break-all; display: flex; align-items: center; justify-content: space-between; gap: 8px;">
              <span style="min-width: 0;">${escapeHtml(invoice.uri)}</span>
              <button data-imoney="copy" style="background: none; border: 1px solid #0f2e23; color: #0f2e23; font-family: ${mono}; font-size: 10.5px; letter-spacing: 0.1em; text-transform: uppercase; padding: 3px 7px; cursor: pointer; flex: none;">Copy</button>
            </div>
          </div>

          <div data-imoney="status" role="status" style="font-size: 14px; color: #3d6652; font-weight: 500;">Waiting for payment (<span data-imoney="countdown">${timeoutSeconds}</span>s)</div>
        </div>
      </div>
    `;
    document.body.appendChild(overlay);
    const part = (name: string) => overlay.querySelector(`[data-imoney="${name}"]`) as HTMLElement | null;

    const abort = new AbortController();
    let remaining = timeoutSeconds;
    const countdown = setInterval(() => {
      remaining -= 1;
      const el = part('countdown');
      if (el) el.innerText = String(Math.max(remaining, 0));
    }, 1000);
    let closed = false;
    const close = () => {
      if (closed) return;
      closed = true;
      clearInterval(countdown);
      abort.abort();
      overlay.remove();
    };

    const copyBtn = part('copy');
    if (copyBtn) {
      copyBtn.onclick = () => {
        void navigator.clipboard.writeText(invoice.uri);
        copyBtn.innerText = 'Copied!';
        setTimeout(() => (copyBtn.innerText = 'Copy'), 2000);
      };
    }
    const closeBtn = part('close');
    if (closeBtn) {
      closeBtn.onclick = () => {
        close();
        onCancel?.();
      };
    }

    let seenReported = false;
    this.waitForPayment(invoice, {
      level,
      timeoutMs: timeoutSeconds * 1000,
      signal: abort.signal,
      onProgress: (status) => {
        const statusEl = part('status');
        if (status.seen_atoms >= invoice.amountAtoms && !seenReported) {
          seenReported = true;
          onSeen?.(status);
        }
        if (!statusEl || !seenReported) return;
        statusEl.style.color = '#94580a';
        statusEl.innerText = status.network_alert
          ? 'Payment seen. The network is unsettled, so confirmation is taking longer.'
          : status.included_atoms >= invoice.amountAtoms
            ? 'Payment is in a block. Waiting for it to be buried deeper.'
            : 'Payment seen. Waiting for it to enter a block.';
      },
    }).then(
      (status) => {
        const statusEl = part('status');
        if (statusEl) {
          statusEl.style.color = '#0b7a4b';
          statusEl.innerText = level === 'seen' ? 'Payment seen.' : level === 'final' ? 'Payment is final.' : 'Payment received.';
        }
        setTimeout(() => {
          close();
          const atoms = atomsAt(status, level);
          onSuccess?.({
            invoiceId: invoice.invoiceId,
            txIds: status.payments.map((p) => p.tx_id),
            atoms,
            imn: atoms / ATOMS_PER_IMN,
            level,
          });
        }, 1200);
      },
      (error: Error) => {
        if (closed) return;
        close();
        if (/Timed out/.test(error.message)) onExpire?.();
      }
    );

    return { invoice, close };
  }
}
