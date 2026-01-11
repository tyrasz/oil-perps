import { useMemo } from 'react';
import { Program, AnchorProvider } from '@project-serum/anchor';
import type { Idl } from '@project-serum/anchor';
import { useConnection, useWallet } from '@solana/wallet-adapter-react';
import { PERPS_CORE_PROGRAM_ID } from '../config/program';
import { IDL } from '../../../target/types/perps_core';

/**
 * Hook to get the Anchor Program instance for perps-core
 * Returns null if wallet is not connected
 */
export function usePerpsProgram() {
  const { connection } = useConnection();
  const wallet = useWallet();

  const provider = useMemo(() => {
    if (!wallet.publicKey || !wallet.signTransaction || !wallet.signAllTransactions) {
      return null;
    }

    return new AnchorProvider(
      connection,
      {
        publicKey: wallet.publicKey,
        signTransaction: wallet.signTransaction,
        signAllTransactions: wallet.signAllTransactions,
      },
      { commitment: 'confirmed' }
    );
  }, [connection, wallet.publicKey, wallet.signTransaction, wallet.signAllTransactions]);

  const program = useMemo(() => {
    if (!provider) return null;

    // Cast IDL to work with the Program constructor
    return new Program(
      IDL as unknown as Idl,
      PERPS_CORE_PROGRAM_ID,
      provider
    );
  }, [provider]);

  return {
    program,
    provider,
    connected: !!wallet.publicKey,
    publicKey: wallet.publicKey,
  };
}
