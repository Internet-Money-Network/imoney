// SPDX-License-Identifier: MIT
pragma solidity 0.8.26;

/// Tokens for the tests only: an ordinary one, and one that answers `false` instead of
/// reverting when a transfer fails.
contract TestToken {
    mapping(address => uint256) public balanceOf;
    mapping(address => mapping(address => uint256)) public allowance;
    bool public immutable lenient;

    constructor(bool lenient_) {
        lenient = lenient_;
        balanceOf[msg.sender] = 1_000_000;
    }

    function approve(address spender, uint256 amount) external returns (bool) {
        allowance[msg.sender][spender] = amount;
        return true;
    }

    function transfer(address to, uint256 amount) external returns (bool) {
        return move(msg.sender, to, amount);
    }

    function transferFrom(address from, address to, uint256 amount) external returns (bool) {
        if (allowance[from][msg.sender] < amount) {
            if (lenient) return false;
            revert("allowance");
        }
        allowance[from][msg.sender] -= amount;
        return move(from, to, amount);
    }

    function move(address from, address to, uint256 amount) private returns (bool) {
        if (balanceOf[from] < amount) {
            if (lenient) return false;
            revert("balance");
        }
        balanceOf[from] -= amount;
        balanceOf[to] += amount;
        return true;
    }
}

/// A recipient that tries to claim again from inside its own payment.
contract Reentrant {
    address public swap;
    bytes32 public id;
    bytes32 public secret;
    bool public reentryWorked;

    function aim(address swap_, bytes32 id_, bytes32 secret_) external {
        (swap, id, secret) = (swap_, id_, secret_);
    }

    receive() external payable {
        (bool ok,) = swap.call(abi.encodeWithSignature("claim(bytes32,bytes32)", id, secret));
        if (ok) reentryWorked = true;
    }
}
