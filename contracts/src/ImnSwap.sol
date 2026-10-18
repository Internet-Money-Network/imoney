// SPDX-License-Identifier: MIT
pragma solidity 0.8.26;

/// The Ethereum-side half of an atomic swap with Internet Money (IMN).
///
/// Ether or tokens locked here can leave in exactly two ways: to the recipient, when anyone
/// presents the 32-byte secret whose SHA-256 hash the lock names; or back to the sender, once
/// the lock's deadline has passed. An Internet Money hash-lock address works the same way
/// with the same hash, so two people who lock for each other can trade without a custodian:
/// claiming one side publishes the secret that claims the other.
///
/// The contract has no owner, no fees, no upgrade path and no way to pause. It holds only
/// what is locked in it, and each lock can be settled once.
///
/// Not audited. Tokens that take a fee on transfer or change balances by themselves are not
/// supported: a lock records what actually arrived, and pays out that amount.
contract ImnSwap {
    struct Lock {
        address sender;
        address recipient;
        /// The token locked, or the zero address for ether.
        address token;
        uint256 amount;
        /// SHA-256 of the secret.
        bytes32 hash;
        /// Unix time from which the sender may take the lock back.
        uint64 refundAfter;
        bool settled;
    }

    mapping(bytes32 => Lock) public locks;
    uint256 private nextNonce;

    event Locked(
        bytes32 indexed id,
        address indexed sender,
        address indexed recipient,
        address token,
        uint256 amount,
        bytes32 hash,
        uint64 refundAfter
    );
    /// A lock that buys IMN from a swap offer: the buyer's IMN key, and the blue score from
    /// which the seller may take back the IMN they lock for it. The seller's software reads this
    /// to fill the offer, and the buyer's wallet to find the IMN lock.
    event ImnOrder(bytes32 indexed id, bytes32 imnKey, uint64 imnRefundAfter);
    /// The secret is published here for the other party of the swap to read.
    event Claimed(bytes32 indexed id, bytes32 secret);
    event Refunded(bytes32 indexed id);

    error NothingToLock();
    error BadRecipient();
    error DeadlineInThePast();
    error WrongValue();
    error UnknownLock();
    error AlreadySettled();
    error WrongSecret();
    error TooEarly();
    error TransferFailed();

    /// Locks ether (`token` zero, `amount` equal to the value sent) or tokens (approved to
    /// this contract beforehand) for `recipient`. Returns the lock's id.
    function lock(address recipient, bytes32 hash, uint64 refundAfter, address token, uint256 amount)
        external
        payable
        returns (bytes32 id)
    {
        id = lockFunds(recipient, hash, refundAfter, token, amount);
    }

    /// The same lock, made to buy IMN from a swap offer: also names the buyer's IMN key and
    /// the blue score from which the seller may take back the IMN they lock in return.
    function lockForImn(
        address recipient,
        bytes32 hash,
        uint64 refundAfter,
        address token,
        uint256 amount,
        bytes32 imnKey,
        uint64 imnRefundAfter
    ) external payable returns (bytes32 id) {
        id = lockFunds(recipient, hash, refundAfter, token, amount);
        emit ImnOrder(id, imnKey, imnRefundAfter);
    }

    function lockFunds(address recipient, bytes32 hash, uint64 refundAfter, address token, uint256 amount)
        private
        returns (bytes32 id)
    {
        if (recipient == address(0)) revert BadRecipient();
        if (refundAfter <= block.timestamp) revert DeadlineInThePast();

        if (token == address(0)) {
            if (msg.value != amount) revert WrongValue();
        } else {
            if (msg.value != 0) revert WrongValue();
            // Record what arrived, not what was asked for
            uint256 before = balanceOf(token);
            callToken(token, abi.encodeWithSelector(0x23b872dd, msg.sender, address(this), amount)); // transferFrom
            amount = balanceOf(token) - before;
        }
        if (amount == 0) revert NothingToLock();

        id = keccak256(abi.encode(block.chainid, address(this), msg.sender, nextNonce++));
        locks[id] = Lock(msg.sender, recipient, token, amount, hash, refundAfter, false);
        emit Locked(id, msg.sender, recipient, token, amount, hash, refundAfter);
    }

    /// Pays the lock to its recipient. Anyone may call it; only the secret matters. A claim
    /// is accepted for as long as the lock has not been refunded.
    function claim(bytes32 id, bytes32 secret) external {
        Lock storage l = locks[id];
        if (l.sender == address(0)) revert UnknownLock();
        if (l.settled) revert AlreadySettled();
        if (sha256(abi.encodePacked(secret)) != l.hash) revert WrongSecret();

        l.settled = true;
        emit Claimed(id, secret);
        pay(l.token, l.recipient, l.amount);
    }

    /// Returns the lock to its sender once the deadline has passed. Anyone may call it.
    function refund(bytes32 id) external {
        Lock storage l = locks[id];
        if (l.sender == address(0)) revert UnknownLock();
        if (l.settled) revert AlreadySettled();
        if (block.timestamp < l.refundAfter) revert TooEarly();

        l.settled = true;
        emit Refunded(id);
        pay(l.token, l.sender, l.amount);
    }

    // The lock is marked settled before anything is sent, so a recipient that calls back in
    // finds nothing left to take.
    function pay(address token, address to, uint256 amount) private {
        if (token == address(0)) {
            (bool sent,) = to.call{value: amount}("");
            if (!sent) revert TransferFailed();
        } else {
            callToken(token, abi.encodeWithSelector(0xa9059cbb, to, amount)); // transfer
        }
    }

    // Some tokens return nothing and some return false instead of reverting; accept the
    // first and refuse the second.
    function callToken(address token, bytes memory data) private {
        if (token.code.length == 0) revert TransferFailed();
        (bool ok, bytes memory result) = token.call(data);
        if (!ok || (result.length != 0 && !abi.decode(result, (bool)))) revert TransferFailed();
    }

    function balanceOf(address token) private view returns (uint256) {
        (bool ok, bytes memory result) = token.staticcall(abi.encodeWithSelector(0x70a08231, address(this)));
        if (!ok || result.length < 32) revert TransferFailed();
        return abi.decode(result, (uint256));
    }
}
