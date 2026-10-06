export const SOMPI_PER_IM: bigint;

export interface IMoneyClientConfig {
  nodeUrl?: string;
  network?: 'mainnet' | 'testnet';
}

export interface NodeInfo {
  network: string;
  total_blocks: number;
  virtual_selected_parent: string;
  virtual_blue_score: number;
  virtual_daa_score: number;
  tips: string[];
  current_bits: string;
  current_block_reward_im: number;
  target_block_interval_sec: number;
  mining_address?: string;
  mempool_size: number;
}

export interface AddressBalance {
  address: string;
  balance_im: number;
  balance_atoms: number;
}

export interface UtxoItem {
  transaction_id: string;
  index: number;
  value_atoms: number;
  value_im: number;
}

export interface PaymentInvoiceOptions {
  merchantAddress: string;
  amountIm: number;
  orderId?: string;
  memo?: string;
  timeoutSeconds?: number;
  onSuccess?: (payment: { txId?: string; atoms: number; im: number }) => void;
  onExpire?: () => void;
  onCancel?: () => void;
}

export class IMoneyClient {
  readonly nodeUrl: string;
  readonly network: 'mainnet' | 'testnet';
  constructor(config?: IMoneyClientConfig);
  getInfo(): Promise<NodeInfo>;
  getBalance(address: string): Promise<AddressBalance>;
  getUtxos(address: string): Promise<UtxoItem[]>;
  subscribeAddressPayments(
    address: string,
    onPayment: (data: { change_atoms: number; balance_atoms: number; balance_im: number }) => void
  ): () => void;
  openCheckoutModal(options: PaymentInvoiceOptions): void;
}
