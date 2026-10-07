export const ATOMS_PER_IMN: bigint;

export interface IMoneyClientConfig {
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
  mining_address?: string;
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
  readonly nodeUrl: string;
  readonly network: 'mainnet' | 'testnet';
  constructor(config?: IMoneyClientConfig);
  getInfo(): Promise<NodeInfo>;
  getBalance(address: string): Promise<AddressBalance>;
  getUtxos(address: string): Promise<UtxoItem[]>;
  getTxStatus(txId: string): Promise<TxStatus>;
  subscribeAddressPayments(
    address: string,
    onPayment: (data: { change_atoms: number; balance_atoms: number; balance_imn: number }) => void
  ): () => void;
  openCheckoutModal(options: PaymentInvoiceOptions): void;
}

