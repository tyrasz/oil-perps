import type { Connection, TransactionSignature } from '@solana/web3.js';
import { errorName, explainProgramError, programErrorCode, type ErrorContext } from './errors';

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
 * Parse Anchor/program errors to user-friendly messages. `context` says what
 * the user was doing, so pool errors can say what to do next.
 */
export function parseAnchorError(error: unknown, context: ErrorContext = {}): string {
  const code = programErrorCode(error);
  if (code !== undefined) {
    const name = errorName(code);
    return name ? explainProgramError(name, context) : `Program error: ${code}`;
  }

  if (error && typeof error === 'object') {
    const err = error as Record<string, unknown>;

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
