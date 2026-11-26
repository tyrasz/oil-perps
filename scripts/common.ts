// Shared by the deploy scripts (initialize-market.ts, oracle-feed.ts): program
// ids, PDAs, the admin keypair and the typed perps-core program.
import * as anchor from '@anchor-lang/core';
import { Program } from '@anchor-lang/core';
// Generated with the IDL by scripts/gen-idl.sh (target/types/perps_core.ts)
import type { PerpsCore } from '../target/types/perps_core';
import { Connection, Keypair, PublicKey, SYSVAR_CLOCK_PUBKEY } from '@solana/web3.js';
import * as fs from 'fs';
import * as path from 'path';

export const PERPS_CORE_PROGRAM_ID = new PublicKey('Fg6PaFpoGXkYsidMpWTK6W2BeZ7FEfcYkg476zPFsLnS');
export const BPF_LOADER_UPGRADEABLE = new PublicKey('BPFLoaderUpgradeab1e11111111111111111111111');

// USDC Mint (Testnet)
export const USDC_MINT = new PublicKey('4zMMC9srt5Ri5X14GAgXhaHii3GnPAEERYPJgZJDncDU');

export function getConfigPDA(): [PublicKey, number] {
  return PublicKey.findProgramAddressSync([Buffer.from('config')], PERPS_CORE_PROGRAM_ID);
}

export function getProgramDataPDA(): [PublicKey, number] {
  return PublicKey.findProgramAddressSync([PERPS_CORE_PROGRAM_ID.toBuffer()], BPF_LOADER_UPGRADEABLE);
}

// One market per (collateral mint, commodity)
export function getMarketPDA(collateralMint: PublicKey, commodity: string): [PublicKey, number] {
  return PublicKey.findProgramAddressSync(
    [Buffer.from('market'), collateralMint.toBuffer(), Buffer.from(commodity)],
    PERPS_CORE_PROGRAM_ID
  );
}

export function getLpPoolPDA(market: PublicKey): [PublicKey, number] {
  return PublicKey.findProgramAddressSync([Buffer.from('lp_pool'), market.toBuffer()], PERPS_CORE_PROGRAM_ID);
}

export async function loadKeypair(): Promise<Keypair> {
  const keypairPath = process.env.ANCHOR_WALLET ||
    path.join(process.env.HOME || '', '.config/solana/id.json');

  const keypairData = JSON.parse(fs.readFileSync(keypairPath, 'utf-8'));
  return Keypair.fromSecretKey(Uint8Array.from(keypairData));
}

/** perps-core from target/idl/perps_core.json; exits if the IDL is missing or for another program id. */
export function loadPerpsProgram(connection: Connection, authority: Keypair): Program<PerpsCore> {
  const wallet = new anchor.Wallet(authority);
  const provider = new anchor.AnchorProvider(connection, wallet, {
    commitment: 'confirmed',
  });
  const idlPath = path.join(__dirname, '../target/idl/perps_core.json');
  if (!fs.existsSync(idlPath)) {
    console.error(`IDL not found at ${idlPath}`);
    console.log('Run: scripts/gen-idl.sh');
    process.exit(1);
  }
  const idl = JSON.parse(fs.readFileSync(idlPath, 'utf-8')) as PerpsCore;
  if (idl.address !== PERPS_CORE_PROGRAM_ID.toBase58()) {
    console.error(`IDL address ${idl.address} is not ${PERPS_CORE_PROGRAM_ID}`);
    process.exit(1);
  }
  // @anchor-lang/core takes the program id from the IDL's `address`
  return new Program<PerpsCore>(idl, provider);
}

/** The cluster's Clock::unix_timestamp (what the timelock and read_price compare against). */
export async function chainTime(connection: Connection): Promise<number> {
  const info = await connection.getAccountInfo(SYSVAR_CLOCK_PUBKEY);
  if (!info) throw new Error('Clock sysvar not found');
  return Number(info.data.readBigInt64LE(32));
}
