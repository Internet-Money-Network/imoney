/**
 * Internet Money (IMN) SDK for the browser and Node.js.
 *
 * Talks to an Internet Money node, creates invoices, and tells you when an invoice has been
 * paid and how settled the payment is. It never handles private keys.
 */
/** 1 IMN = 10^8 atoms. */
export declare const ATOMS_PER_IMN = 100000000;
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
    onSuccess?: (payment: {
        invoiceId: string;
        txIds: string[];
        atoms: number;
        imn: number;
        level: PaymentLevel;
    }) => void;
    onExpire?: () => void;
    onCancel?: () => void;
}
/** An invoice ID is 1 to 64 characters from `A-Z a-z 0-9 - _ .` */
export declare function isValidInvoiceId(id: string): boolean;
/** Converts an IMN amount to atoms exactly, without floating-point rounding. */
export declare function imnToAtoms(amount: number | string): number;
/** Formats atoms as an IMN amount with trailing zeros removed. */
export declare function atomsToImn(atoms: number): string;
/** A random invoice ID. Prefer your own order number when you have one. */
export declare function newInvoiceId(prefix?: string): string;
/** Builds the text a wallet scans: `imn:q...?amount=12.5&invoice=INV-1042`. */
export declare function paymentUri(address: string, amountImn: string, invoiceId?: string): string;
/** Reads a payment URI back into its parts. A bare address is accepted too. */
export declare function parsePaymentUri(uri: string): {
    address: string;
    amountImn?: string;
    invoiceId?: string;
};
/** Renders text as a QR code, returned as an SVG string. Generated locally; nothing is sent anywhere. */
export declare function qrSvg(text: string, cellSize?: number, margin?: number): string;
export declare class IMoneyClient {
    readonly nodeUrl: string;
    readonly network: 'mainnet' | 'testnet';
    constructor(config?: IMoneyClientConfig);
    private get;
    /** Fetch current network status and blockDAG metrics. */
    getInfo(): Promise<NodeInfo>;
    /** Retrieve the balance of any IMN address. */
    getBalance(address: string): Promise<AddressBalance>;
    /** Retrieve the unspent outputs of an address. */
    getUtxos(address: string): Promise<UtxoItem[]>;
    /** Query a transaction's status and confirmations. */
    getTxStatus(txId: string): Promise<TxStatus>;
    /** Submit a signed transaction (as produced by the wallet's signing module). */
    broadcastTransaction(transaction: unknown): Promise<{
        success: boolean;
        tx_id?: string;
        error?: string;
    }>;
    /**
     * Describes a payment you expect. Nothing is sent to the node: an invoice exists once a
     * payment naming its ID arrives. Use an ID that is unique per order.
     */
    createInvoice(options: {
        address: string;
        amountImn: number | string;
        invoiceId?: string;
    }): Invoice;
    /** What has been paid towards an invoice so far. */
    getInvoice(invoiceId: string, address: string): Promise<InvoiceStatus>;
    /**
     * Resolves once the invoice has been paid in full at the requested level, and rejects on
     * timeout or abort. The node's own records decide; a push from the node only makes the next
     * check happen sooner.
     */
    waitForPayment(invoice: Invoice, options?: WaitOptions): Promise<InvoiceStatus>;
    /**
     * Listens for activity on an address over WebSocket. `onEvent` receives `payment_seen` when a
     * payment reaches the node and `payment_received` when the balance changes. Returns a function
     * that stops listening. Where WebSocket is unavailable it does nothing; polling still works.
     */
    subscribeAddressPayments(address: string, onEvent: (data: any) => void): () => void;
    /**
     * Opens a checkout window for one invoice: amount, QR code, address and live status.
     * Browser only. The payment is matched by invoice ID and amount, never by balance alone.
     */
    openCheckoutModal(options: CheckoutOptions): {
        invoice: Invoice;
        close: () => void;
    };
}
