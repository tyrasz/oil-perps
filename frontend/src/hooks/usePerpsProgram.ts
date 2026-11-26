import { useMemo } from 'react';
import { Program, AnchorProvider } from '@anchor-lang/core';
import { useConnection, useWallet } from '@solana/wallet-adapter-react';
import { PERPS_CORE_PROGRAM_ID } from '../config/program';
// perps-core's IDL (spec 0.1.0) and its TS type: committed copies of
// target/idl/perps_core.json and target/types/perps_core.ts, refreshed by
// `scripts/gen-idl.sh --sync` and checked in CI (docs/lp-pool.md D49)
import perpsCoreIdl from '../idl/perps_core.json';
import type { PerpsCore } from '../idl/perps_core';

/**
 * The IDL at PERPS_CORE_PROGRAM_ID: @anchor-lang/core takes the program id
 * from the IDL's `address`.
 */
const IDL = { ...perpsCoreIdl, address: PERPS_CORE_PROGRAM_ID.toBase58() } as PerpsCore;

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
    return new Program<PerpsCore>(IDL, provider);
  }, [provider]);

  return {
    program,
    provider,
    connected: !!wallet.publicKey,
    publicKey: wallet.publicKey,
  };
}
