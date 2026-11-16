import * as anchor from '@coral-xyz/anchor';
import { Program } from '@coral-xyz/anchor';
import { PublicKey, Keypair, Connection } from '@solana/web3.js';
import { TOKEN_PROGRAM_ID } from '@solana/spl-token';
import * as fs from 'fs';
import * as path from 'path';

// Program ID
const PERPS_CORE_PROGRAM_ID = new PublicKey('Fg6PaFpoGXkYsidMpWTK6W2BeZ7FEfcYkg476zPFsLnS');

// USDC Mint (Testnet)
const USDC_MINT = new PublicKey('4zMMC9srt5Ri5X14GAgXhaHii3GnPAEERYPJgZJDncDU');

// Pyth Price Feeds (Testnet)
const PYTH_PRICE_FEEDS: Record<string, PublicKey> = {
  OIL: new PublicKey('GVXRSBjFk6e6J3NbVPXohDJetcTjaeeuykUpbQF8UoMU'),
  GOLD: new PublicKey('sXgHcPCNsXM8KaC3CXNXQS8qprLR4dVxQyJyxNmBsLR'),
  SILVER: new PublicKey('77JipqJaP9LPFyEGjT2zqz5qxL6KBx3nVtbMd1PPdPr9'),
  NATGAS: new PublicKey('DBE3N8uNjhKPNAR4oJT8vKwZQN5yDsRXGBCQu4k3Gfgr'),
  COPPER: new PublicKey('4wxQsP2B7HNyH4sH3n2J1oKeW6vPExU6mBLgPswN8pqZ'),
};

// Market configurations per commodity
const MARKET_CONFIGS: Record<string, {
  maxLeverage: number;
  maintenanceMarginRatio: number;
  initialMarginRatio: number;
  takerFee: number;
  makerFee: number;
  liquidationFee: number;
  maxOpenInterest: number;
  fundingInterval: number;
}> = {
  OIL: {
    maxLeverage: 20000, // 20x (3 decimals)
    maintenanceMarginRatio: 500, // 0.5% (3 decimals)
    initialMarginRatio: 1000, // 1% (3 decimals)
    takerFee: 50, // 0.05% (3 decimals)
    makerFee: 20, // 0.02% (3 decimals)
    liquidationFee: 100, // 0.1% (3 decimals)
    maxOpenInterest: 10_000_000_000_000, // 10M USDC (6 decimals)
    fundingInterval: 3600, // 1 hour
  },
  GOLD: {
    maxLeverage: 20000,
    maintenanceMarginRatio: 500,
    initialMarginRatio: 1000,
    takerFee: 50,
    makerFee: 20,
    liquidationFee: 100,
    maxOpenInterest: 10_000_000_000_000,
    fundingInterval: 3600,
  },
  SILVER: {
    maxLeverage: 15000, // 15x
    maintenanceMarginRatio: 600,
    initialMarginRatio: 1200,
    takerFee: 50,
    makerFee: 20,
    liquidationFee: 100,
    maxOpenInterest: 5_000_000_000_000,
    fundingInterval: 3600,
  },
  NATGAS: {
    maxLeverage: 10000, // 10x
    maintenanceMarginRatio: 800,
    initialMarginRatio: 1500,
    takerFee: 60,
    makerFee: 30,
    liquidationFee: 150,
    maxOpenInterest: 5_000_000_000_000,
    fundingInterval: 3600,
  },
  COPPER: {
    maxLeverage: 15000,
    maintenanceMarginRatio: 600,
    initialMarginRatio: 1200,
    takerFee: 50,
    makerFee: 20,
    liquidationFee: 100,
    maxOpenInterest: 5_000_000_000_000,
    fundingInterval: 3600,
  },
};

// PDA derivation functions
function getMarketPDA(collateralMint: PublicKey): [PublicKey, number] {
  return PublicKey.findProgramAddressSync(
    [Buffer.from('market'), collateralMint.toBuffer()],
    PERPS_CORE_PROGRAM_ID
  );
}

function getVaultPDA(market: PublicKey): [PublicKey, number] {
  return PublicKey.findProgramAddressSync(
    [Buffer.from('vault'), market.toBuffer()],
    PERPS_CORE_PROGRAM_ID
  );
}

function getVaultTokenAccountPDA(market: PublicKey): [PublicKey, number] {
  return PublicKey.findProgramAddressSync(
    [Buffer.from('vault_token'), market.toBuffer()],
    PERPS_CORE_PROGRAM_ID
  );
}

async function loadKeypair(): Promise<Keypair> {
  const keypairPath = process.env.ANCHOR_WALLET ||
    path.join(process.env.HOME || '', '.config/solana/id.json');

  const keypairData = JSON.parse(fs.readFileSync(keypairPath, 'utf-8'));
  return Keypair.fromSecretKey(Uint8Array.from(keypairData));
}

async function initializeMarket(
  program: Program,
  authority: Keypair,
  commodity: string
): Promise<string> {
  const config = MARKET_CONFIGS[commodity];
  if (!config) {
    throw new Error(`Unknown commodity: ${commodity}`);
  }

  const pythPriceFeed = PYTH_PRICE_FEEDS[commodity];
  if (!pythPriceFeed) {
    throw new Error(`No Pyth price feed for commodity: ${commodity}`);
  }

  const [marketPda] = getMarketPDA(USDC_MINT);
  const [vaultPda] = getVaultPDA(marketPda);
  const [vaultTokenAccountPda] = getVaultTokenAccountPDA(marketPda);

  console.log(`Initializing market for ${commodity}...`);
  console.log(`  Market PDA: ${marketPda.toString()}`);
  console.log(`  Vault PDA: ${vaultPda.toString()}`);
  console.log(`  Vault Token Account: ${vaultTokenAccountPda.toString()}`);
  console.log(`  Pyth Price Feed: ${pythPriceFeed.toString()}`);

  const params = {
    commodity,
    maxLeverage: config.maxLeverage,
    maintenanceMarginRatio: config.maintenanceMarginRatio,
    initialMarginRatio: config.initialMarginRatio,
    takerFee: config.takerFee,
    makerFee: config.makerFee,
    liquidationFee: config.liquidationFee,
    maxOpenInterest: new anchor.BN(config.maxOpenInterest),
    fundingInterval: new anchor.BN(config.fundingInterval),
  };

  const tx = await program.methods
    .initializeMarket(params)
    .accounts({
      authority: authority.publicKey,
      market: marketPda,
      vault: vaultPda,
      vaultTokenAccount: vaultTokenAccountPda,
      collateralMint: USDC_MINT,
      pythPriceFeed,
      systemProgram: anchor.web3.SystemProgram.programId,
      tokenProgram: TOKEN_PROGRAM_ID,
      rent: anchor.web3.SYSVAR_RENT_PUBKEY,
    })
    .signers([authority])
    .rpc();

  console.log(`Market initialized! Transaction: ${tx}`);
  return tx;
}

async function checkMarketExists(connection: Connection): Promise<boolean> {
  const [marketPda] = getMarketPDA(USDC_MINT);
  const accountInfo = await connection.getAccountInfo(marketPda);
  return accountInfo !== null;
}

async function main() {
  const commodity = process.argv[2] || 'OIL';
  const rpcUrl = process.env.RPC_URL || 'https://api.testnet.solana.com';

  console.log(`\n=== Initialize Market Script ===`);
  console.log(`Network: ${rpcUrl}`);
  console.log(`Commodity: ${commodity}`);
  console.log(`Program ID: ${PERPS_CORE_PROGRAM_ID.toString()}`);
  console.log(`USDC Mint: ${USDC_MINT.toString()}\n`);

  // Load keypair
  const authority = await loadKeypair();
  console.log(`Authority: ${authority.publicKey.toString()}`);

  // Setup connection and provider
  const connection = new Connection(rpcUrl, 'confirmed');
  const wallet = new anchor.Wallet(authority);
  const provider = new anchor.AnchorProvider(connection, wallet, {
    commitment: 'confirmed',
  });

  // Check balance
  const balance = await connection.getBalance(authority.publicKey);
  console.log(`Balance: ${balance / 1e9} SOL\n`);

  if (balance < 0.1 * 1e9) {
    console.error('Insufficient balance! Need at least 0.1 SOL');
    console.log('Run: solana airdrop 1 --url testnet');
    process.exit(1);
  }

  // Check if market already exists
  const marketExists = await checkMarketExists(connection);
  if (marketExists) {
    console.log('Market already exists!');
    const [marketPda] = getMarketPDA(USDC_MINT);
    console.log(`Market address: ${marketPda.toString()}`);
    process.exit(0);
  }

  // Load IDL
  const idlPath = path.join(__dirname, '../target/idl/perps_core.json');
  if (!fs.existsSync(idlPath)) {
    console.error(`IDL not found at ${idlPath}`);
    console.log('Run: anchor build');
    process.exit(1);
  }

  const idl = JSON.parse(fs.readFileSync(idlPath, 'utf-8'));
  const program = new Program(idl, PERPS_CORE_PROGRAM_ID, provider);

  // Initialize market
  try {
    const signature = await initializeMarket(program, authority, commodity);
    console.log(`\nSuccess! View transaction:`);
    console.log(`https://explorer.solana.com/tx/${signature}?cluster=testnet`);
  } catch (error) {
    console.error('Failed to initialize market:', error);
    process.exit(1);
  }
}

main().catch(console.error);
