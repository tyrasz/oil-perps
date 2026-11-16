import type { Connection, TransactionSignature } from '@solana/web3.js';

/**
 * Wait for transaction confirmation with status updates
 */
export async function confirmTransaction(
  connection: Connection,
  signature: TransactionSignature,
  onStatus?: (status: string) => void
): Promise<void> {
  onStatus?.('Confirming transaction...');

  const latestBlockHash = await connection.getLatestBlockhash();

  await connection.confirmTransaction({
    signature,
    blockhash: latestBlockHash.blockhash,
    lastValidBlockHeight: latestBlockHash.lastValidBlockHeight,
  });

  onStatus?.('Transaction confirmed!');
}

/**
 * Parse Anchor/program errors to user-friendly messages
 */
export function parseAnchorError(error: unknown): string {
  // Handle Anchor program errors
  if (error && typeof error === 'object') {
    const err = error as Record<string, unknown>;

    // Check for Anchor error code
    if (err.code && typeof err.code === 'number') {
      const errorMessages: Record<number, string> = {
        6000: 'Market is currently paused',
        6001: 'Leverage exceeds maximum allowed',
        6002: 'Insufficient collateral. Please deposit more USDC first.',
        6003: 'Position size is too small',
        6004: 'Position size exceeds maximum',
        6005: 'Open interest cap would be exceeded',
        6006: 'Position is below maintenance margin',
        6007: 'Position is not liquidatable',
        6008: 'Invalid oracle price',
        6009: 'Oracle price is stale. Please try again.',
        6010: 'Invalid position side',
        6011: 'Position not found',
        6012: 'Position is already closed',
        6013: 'Unauthorized access',
        6014: 'Math overflow error',
        6015: 'Invalid market configuration',
        6016: 'Cannot reduce position below zero',
        6017: 'Insufficient vault balance',
      };

      return errorMessages[err.code] || `Program error: ${err.code}`;
    }

    // Check for error message
    if (err.message && typeof err.message === 'string') {
      // Common wallet errors
      if (err.message.includes('User rejected')) {
        return 'Transaction was cancelled';
      }
      if (err.message.includes('insufficient funds')) {
        return 'Insufficient SOL for transaction fees';
      }
      if (err.message.includes('Blockhash not found')) {
        return 'Transaction expired. Please try again.';
      }

      return err.message;
    }

    // Check for logs array (simulation errors)
    if (err.logs && Array.isArray(err.logs)) {
      const logs = err.logs as string[];
      const errorLog = logs.find(log => log.includes('Error'));
      if (errorLog) {
        return errorLog;
      }
    }
  }

  // Fallback
  if (error instanceof Error) {
    return error.message;
  }

  return 'Transaction failed. Please try again.';
}

/**
 * Format transaction signature for display
 */
export function formatSignature(signature: string, length: number = 8): string {
  if (signature.length <= length * 2) return signature;
  return `${signature.slice(0, length)}...${signature.slice(-length)}`;
}

/**
 * Get explorer URL for a transaction
 */
export function getExplorerUrl(
  signature: string,
  network: 'testnet' | 'devnet' | 'mainnet' = 'testnet'
): string {
  const cluster = network === 'mainnet' ? '' : `?cluster=${network}`;
  return `https://explorer.solana.com/tx/${signature}${cluster}`;
}
