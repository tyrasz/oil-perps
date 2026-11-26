import { useCallback, useMemo, useState } from 'react';
import { useConnection } from '@solana/wallet-adapter-react';
import { PublicKey, SystemProgram } from '@solana/web3.js';
import { TOKEN_PROGRAM_ID, getAssociatedTokenAddress } from '@solana/spl-token';
import { BN } from '@anchor-lang/core';
import { usePerpsProgram } from './usePerpsProgram';
import {
  getUserAccountPDA,
  getCommodityMarket,
  getVaultPDA,
  getVaultTokenAccountPDA,
  getPositionPDA,
  getUserReferralPDA,
  getLpPoolPDA,
  commodityForMarket,
  isProgramAccount,
} from '../utils/pda';
import { parseAnchorError, getExplorerUrl } from '../utils/transaction';
import { TradeError, errorCode as errorCodeByName, errorName, programErrorCode, type ErrorContext } from '../utils/errors';
import {
  acceptablePrice,
  closeExecPrice,
  decodeLpPool,
  decodeMarket,
  houseQuote,
  isSettled,
  profitCloseQuietLeft,
  toUnits,
  triggerAcceptablePrice,
} from '../utils/pool';
import { useMarketStore } from '../stores/marketStore';
import {
  PERPS_CORE_PROGRAM_ID,
  USDC_MINT,
  LEVERAGE_DECIMALS,
  DEFAULT_SLIPPAGE_BPS,
  TRIGGER_EXTRA_SLIPPAGE_BPS,
} from '../config/program';

export interface TradingState {
  isLoading: boolean;
  error: string | null;
  lastTxSignature: string | null;
}

interface ReferralCloseAccounts {
  userReferral: PublicKey;
  referralCode: PublicKey;
  referrerTokenAccount: PublicKey;
}

/** Referral accounts for close_position, or null if the trader has no usable referral. */
async function referralAccountsForClose(
  program: NonNullable<ReturnType<typeof usePerpsProgram>['program']>,
  connection: ReturnType<typeof useConnection>['connection'],
  trader: PublicKey,
  collateralMint: PublicKey
): Promise<ReferralCloseAccounts | null> {
  try {
    const [userReferral] = getUserReferralPDA(trader);
    const applied = (await program.account.userReferral.fetchNullable(userReferral)) as { referralCode: PublicKey } | null;
    if (!applied) return null;
    const code = (await program.account.referralCode.fetch(applied.referralCode)) as { owner: PublicKey; isActive: boolean };
    if (!code.isActive) return null;
    const referrerTokenAccount = await getAssociatedTokenAddress(collateralMint, code.owner);
    if (!(await connection.getAccountInfo(referrerTokenAccount))) return null;
    return { userReferral, referralCode: applied.referralCode, referrerTokenAccount };
  } catch (err) {
    console.warn('Referral lookup failed; closing without referral:', err);
    return null;
  }
}

export { TradeError };

export interface OpenOptions {
  /** Worst execution price to accept (6 dp); default: the house quote ± slippage. */
  acceptablePrice?: bigint;
  slippageBps?: number;
}

export interface CloseOptions {
  /** Price the close is quoted at (USD); default: the app's latest price for the commodity. */
  markPrice?: number;
  /** Worst execution price to accept (6 dp); overrides `slippageBps`. */
  acceptablePrice?: bigint;
  slippageBps?: number;
  /**
   * TP/SL: bound the close at `markPrice` (the trigger's price), `max_impact_bps`
   * plus `TRIGGER_EXTRA_SLIPPAGE_BPS` worse, never 0 (§16.5).
   */
  trigger?: boolean;
}

/** The app's latest price (USD) for a commodity, or undefined. */
const latestPrice = (commodity: string | undefined): number | undefined =>
  commodity ? useMarketStore.getState().markets[commodity]?.price || undefined : undefined;

/** USD amount to a u64 BN of 6-dp units, without float artifacts. */
const usdToBn = (amount: number) => new BN(toUnits(amount).toString());

/** The perps-core error name of a failed call, if any. */
function nameOf(error: unknown): string | undefined {
  const code = programErrorCode(error);
  return code === undefined ? undefined : errorName(code);
}

function errorCodeOf(name: string): number {
  return errorCodeByName(name) ?? 0;
}

export function useTrading() {
  const { connection } = useConnection();
  const { program, publicKey, connected } = usePerpsProgram();
  const { selectedCommodity } = useMarketStore();
  // Deposits, withdrawals and new positions go to the selected commodity's market
  const marketPda = useMemo(() => getCommodityMarket(selectedCommodity.id), [selectedCommodity.id]);

  const [state, setState] = useState<TradingState>({
    isLoading: false,
    error: null,
    lastTxSignature: null,
  });

  /**
   * Check if user account exists on-chain
   */
  const checkUserAccountExists = useCallback(async (): Promise<boolean> => {
    if (!publicKey || !connection) return false;

    const [userAccountPda] = getUserAccountPDA(publicKey, marketPda);
    return isProgramAccount(await connection.getAccountInfo(userAccountPda));
  }, [publicKey, connection, marketPda]);

  /**
   * Check if market exists on-chain
   */
  const checkMarketExists = useCallback(async (): Promise<boolean> => {
    if (!connection) return false;

    return isProgramAccount(await connection.getAccountInfo(marketPda));
  }, [connection, marketPda]);

  /**
   * Initialize user account if it doesn't exist
   */
  const initializeUserIfNeeded = useCallback(async (): Promise<void> => {
    if (!program || !publicKey) {
      throw new Error('Wallet not connected');
    }

    const exists = await checkUserAccountExists();
    if (exists) {
      console.log('User account already exists');
      return;
    }

    console.log('Creating user account...');
    const [userAccountPda] = getUserAccountPDA(publicKey, marketPda);

    await program.methods
      .initializeUser()
      .accounts({
        owner: publicKey,
        market: marketPda,
        userAccount: userAccountPda,
        systemProgram: SystemProgram.programId,
      })
      .rpc();

    console.log('User account created');
  }, [program, publicKey, marketPda, checkUserAccountExists]);

  /**
   * Deposit collateral (USDC) to user account
   */
  const depositCollateral = useCallback(
    async (amount: number): Promise<string> => {
      if (!program || !publicKey) {
        throw new Error('Wallet not connected');
      }

      setState(prev => ({ ...prev, isLoading: true, error: null }));

      try {
        await initializeUserIfNeeded();

        const [userAccountPda] = getUserAccountPDA(publicKey, marketPda);
        const [vaultPda] = getVaultPDA(marketPda);
        const [vaultTokenAccountPda] = getVaultTokenAccountPDA(marketPda);

        const userTokenAccount = await getAssociatedTokenAddress(
          USDC_MINT,
          publicKey
        );

        const signature = await program.methods
          .depositCollateral(usdToBn(amount))
          .accounts({
            owner: publicKey,
            userAccount: userAccountPda,
            market: marketPda,
            vault: vaultPda,
            vaultTokenAccount: vaultTokenAccountPda,
            userTokenAccount,
            tokenProgram: TOKEN_PROGRAM_ID,
            lpPool: getLpPoolPDA(marketPda)[0],
          })
          .rpc();

        console.log('Deposit successful:', signature);
        setState(prev => ({
          ...prev,
          isLoading: false,
          lastTxSignature: signature,
        }));

        return signature;
      } catch (error) {
        const errorMessage = parseAnchorError(error, { action: 'deposit' });
        console.error('Deposit failed:', error);
        setState(prev => ({
          ...prev,
          isLoading: false,
          error: errorMessage,
        }));
        throw new Error(errorMessage);
      }
    },
    [program, publicKey, marketPda, initializeUserIfNeeded]
  );

  /**
   * Withdraw collateral (USDC) from user account
   */
  const withdrawCollateral = useCallback(
    async (amount: number): Promise<string> => {
      if (!program || !publicKey) {
        throw new Error('Wallet not connected');
      }

      setState(prev => ({ ...prev, isLoading: true, error: null }));

      try {
        const [userAccountPda] = getUserAccountPDA(publicKey, marketPda);
        const [vaultPda] = getVaultPDA(marketPda);
        const [vaultTokenAccountPda] = getVaultTokenAccountPDA(marketPda);

        const userTokenAccount = await getAssociatedTokenAddress(
          USDC_MINT,
          publicKey
        );

        const signature = await program.methods
          .withdrawCollateral(usdToBn(amount))
          .accounts({
            owner: publicKey,
            userAccount: userAccountPda,
            market: marketPda,
            vault: vaultPda,
            vaultTokenAccount: vaultTokenAccountPda,
            userTokenAccount,
            tokenProgram: TOKEN_PROGRAM_ID,
            lpPool: getLpPoolPDA(marketPda)[0],
          })
          .rpc();

        console.log('Withdraw successful:', signature);
        setState(prev => ({
          ...prev,
          isLoading: false,
          lastTxSignature: signature,
        }));

        return signature;
      } catch (error) {
        const errorMessage = parseAnchorError(error, { action: 'withdraw' });
        console.error('Withdraw failed:', error);
        setState(prev => ({
          ...prev,
          isLoading: false,
          error: errorMessage,
        }));
        throw new Error(errorMessage);
      }
    },
    [program, publicKey, marketPda]
  );

  /**
   * Open a new position against the house (the market's LP pool). The trade
   * executes at the skew price; it fails `SlippageExceeded` beyond
   * `acceptablePrice` (default: the quote at the app's latest price, moved
   * `slippageBps` against the trader).
   */
  const openPosition = useCallback(
    async (
      side: 'long' | 'short',
      size: number,
      leverage: number,
      options: OpenOptions = {}
    ): Promise<string> => {
      if (!program || !publicKey) {
        throw new Error('Wallet not connected');
      }

      setState(prev => ({ ...prev, isLoading: true, error: null }));

      try {
        // Ensure user account exists
        await initializeUserIfNeeded();

        // Get PDAs
        const [userAccountPda] = getUserAccountPDA(publicKey, marketPda);
        const [lpPoolPda] = getLpPoolPDA(marketPda);

        // Market (for total_positions, the position PDA's index) and pool (skew price)
        const [marketAccount, poolAccount] = await Promise.all([
          program.account.market.fetch(marketPda),
          program.account.lpPool.fetch(lpPoolPda),
        ]);
        const positionIndex = (marketAccount.totalPositions as BN).toNumber();

        const [positionPda] = getPositionPDA(publicKey, marketPda, positionIndex);

        // The market's pinned price feed (Market.pyth_price_feed; perps-core
        // refuses any other key, and a feed rotation moves it)
        const pythPriceFeed = marketAccount.pythPriceFeed as PublicKey;

        // Convert parameters
        const sideValue = side === 'long' ? 0 : 1;
        const sizeUnits = toUnits(size);
        const leverageWithDecimals = Math.round(leverage * Math.pow(10, LEVERAGE_DECIMALS));

        let bound = options.acceptablePrice;
        if (bound === undefined) {
          const price = latestPrice(selectedCommodity.id);
          if (!price) throw new Error('No price for this market yet; cannot bound the execution price.');
          const quote = houseQuote(
            decodeMarket(marketAccount as Record<string, unknown>),
            decodeLpPool(poolAccount as Record<string, unknown>),
            side === 'long',
            sizeUnits,
            BigInt(leverageWithDecimals),
            toUnits(price),
            Math.floor(Date.now() / 1000)
          );
          if (!quote.ok) throw new TradeError(parseAnchorError({ code: errorCodeOf(quote.error) }, { action: 'open' }), quote.error);
          bound = acceptablePrice(side === 'long', quote.value.exec, options.slippageBps ?? DEFAULT_SLIPPAGE_BPS);
        }

        console.log('Opening position:', {
          side,
          size,
          leverage,
          positionIndex,
          acceptablePrice: bound.toString(),
          commodity: selectedCommodity.id,
        });

        const signature = await program.methods
          .openPosition({
            side: sideValue,
            size: new BN(sizeUnits.toString()),
            leverage: leverageWithDecimals,
            acceptablePrice: new BN(bound.toString()),
          })
          .accounts({
            owner: publicKey,
            userAccount: userAccountPda,
            market: marketPda,
            position: positionPda,
            pythPriceFeed,
            systemProgram: SystemProgram.programId,
            lpPool: lpPoolPda,
          })
          .rpc();

        console.log('Position opened:', signature);
        console.log('Explorer:', getExplorerUrl(signature, 'testnet'));

        setState(prev => ({
          ...prev,
          isLoading: false,
          lastTxSignature: signature,
        }));

        return signature;
      } catch (error) {
        const errorMessage = error instanceof TradeError ? error.message : parseAnchorError(error, { action: 'open' });
        console.error('Open position failed:', error);
        setState(prev => ({
          ...prev,
          isLoading: false,
          error: errorMessage,
        }));
        throw error instanceof TradeError ? error : new TradeError(errorMessage, nameOf(error));
      }
    },
    [program, publicKey, selectedCommodity, marketPda, initializeUserIfNeeded]
  );

  /**
   * Add collateral from the trader's free balance to an open position.
   */
  const addMargin = useCallback(
    async (positionAddress: string, amount: number): Promise<string> => {
      if (!program || !publicKey) {
        throw new Error('Wallet not connected');
      }

      setState(prev => ({ ...prev, isLoading: true, error: null }));

      try {
        const positionPubkey = new PublicKey(positionAddress);
        const positionAccount = await program.account.position.fetch(positionPubkey);
        const positionMarket = positionAccount.market as PublicKey;
        const [userAccountPda] = getUserAccountPDA(publicKey, positionMarket);

        const signature = await program.methods
          .addMargin(usdToBn(amount))
          .accounts({
            owner: publicKey,
            userAccount: userAccountPda,
            market: positionMarket,
            position: positionPubkey,
            lpPool: getLpPoolPDA(positionMarket)[0],
          })
          .rpc();

        setState(prev => ({ ...prev, isLoading: false, lastTxSignature: signature }));
        return signature;
      } catch (error) {
        const errorMessage = parseAnchorError(error, { action: 'margin' });
        console.error('Add margin failed:', error);
        setState(prev => ({ ...prev, isLoading: false, error: errorMessage }));
        throw new TradeError(errorMessage, nameOf(error));
      }
    },
    [program, publicKey]
  );

  /**
   * Close an existing position
   */
  const closePosition = useCallback(
    async (positionAddress: string, options: CloseOptions = {}): Promise<string> => {
      if (!program || !publicKey) {
        throw new Error('Wallet not connected');
      }

      setState(prev => ({ ...prev, isLoading: true, error: null }));
      // For explaining a failure: the market and pool as fetched for the close
      let fetched: { market: ReturnType<typeof decodeMarket>; pool: ReturnType<typeof decodeLpPool> } | null = null;

      try {
        const positionPubkey = new PublicKey(positionAddress);

        // Fetch position to get market
        const positionAccount = await program.account.position.fetch(positionPubkey);
        const marketPda = positionAccount.market as PublicKey;

        // Market (pyth feed, collateral mint, skew) and the pool (skew price)
        const [lpPoolPda] = getLpPoolPDA(marketPda);
        const [marketAccount, poolAccount] = await Promise.all([
          program.account.market.fetch(marketPda),
          program.account.lpPool.fetch(lpPoolPda),
        ]);
        const market = decodeMarket(marketAccount as Record<string, unknown>);
        const pool = decodeLpPool(poolAccount as Record<string, unknown>);
        fetched = { market, pool };

        // Closing a long sells into the skew, closing a short buys
        const isLong = (positionAccount.side as { long?: object }).long !== undefined;
        let bound = options.acceptablePrice;
        if (bound === undefined) {
          const price = options.markPrice ?? latestPrice(commodityForMarket(marketPda));
          if (!isSettled(market) && !price) throw new Error('No price for this market yet; cannot bound the execution price.');
          if (options.trigger) {
            bound = triggerAcceptablePrice(market, pool, isLong, toUnits(price ?? 0), TRIGGER_EXTRA_SLIPPAGE_BPS);
          } else {
            const exec = closeExecPrice(market, pool, isLong, BigInt((positionAccount.size as BN).toString()), toUnits(price ?? 0));
            if (exec === null || exec <= 0n) throw new Error('Cannot price this close.');
            bound = acceptablePrice(!isLong, exec, options.slippageBps ?? DEFAULT_SLIPPAGE_BPS);
          }
        }

        const [userAccountPda] = getUserAccountPDA(publicKey, marketPda);
        const [vaultPda] = getVaultPDA(marketPda);
        const [vaultTokenAccountPda] = getVaultTokenAccountPDA(marketPda);

        // Get user's token account for receiving settlement
        const userTokenAccount = await getAssociatedTokenAddress(
          marketAccount.collateralMint as PublicKey,
          publicKey
        );

        // With a referral, pass the referral accounts and the referrer's token
        // account: the trader gets the discount and the program pays the
        // referrer at close. Skipped if the referrer has no account for this mint.
        const referral = await referralAccountsForClose(
          program,
          connection,
          publicKey,
          marketAccount.collateralMint as PublicKey
        );

        console.log('Closing position:', positionAddress);

        const signature = await program.methods
          .closePosition(new BN(bound.toString()))
          .accounts({
            owner: publicKey,
            userAccount: userAccountPda,
            market: marketPda,
            position: positionPubkey,
            vault: vaultPda,
            vaultTokenAccount: vaultTokenAccountPda,
            userTokenAccount,
            pythPriceFeed: marketAccount.pythPriceFeed as PublicKey,
            tokenProgram: TOKEN_PROGRAM_ID,
            // Anchor encodes an absent optional account as the program's own id
            userReferral: referral?.userReferral ?? PERPS_CORE_PROGRAM_ID,
            referralCode: referral?.referralCode ?? PERPS_CORE_PROGRAM_ID,
            referrerTokenAccount: referral?.referrerTokenAccount ?? PERPS_CORE_PROGRAM_ID,
            lpPool: lpPoolPda,
          })
          .rpc();

        console.log('Position closed:', signature);
        console.log('Explorer:', getExplorerUrl(signature, 'testnet'));

        setState(prev => ({
          ...prev,
          isLoading: false,
          lastTxSignature: signature,
        }));

        return signature;
      } catch (error) {
        const name = nameOf(error);
        const context: ErrorContext = { action: 'close' };
        if (name === 'ProfitCloseQuiet' && fetched) {
          context.quietLeft = profitCloseQuietLeft(fetched.market, fetched.pool, Math.floor(Date.now() / 1000));
        }
        const errorMessage = parseAnchorError(error, context);
        console.error('Close position failed:', error);
        setState(prev => ({
          ...prev,
          isLoading: false,
          error: errorMessage,
        }));
        throw new TradeError(errorMessage, name, context.quietLeft);
      }
    },
    [program, publicKey, connection]
  );

  /**
   * Clear error state
   */
  const clearError = useCallback(() => {
    setState(prev => ({ ...prev, error: null }));
  }, []);

  return {
    // State
    isLoading: state.isLoading,
    error: state.error,
    lastTxSignature: state.lastTxSignature,
    connected,

    // Actions
    checkUserAccountExists,
    checkMarketExists,
    initializeUserIfNeeded,
    depositCollateral,
    withdrawCollateral,
    openPosition,
    closePosition,
    addMargin,
    clearError,

    // Helpers
    getExplorerUrl: (signature: string) => getExplorerUrl(signature, 'testnet'),
  };
}
