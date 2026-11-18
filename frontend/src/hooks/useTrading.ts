import { useCallback, useMemo, useState } from 'react';
import { useConnection } from '@solana/wallet-adapter-react';
import { PublicKey, SystemProgram } from '@solana/web3.js';
import { TOKEN_PROGRAM_ID, getAssociatedTokenAddress } from '@solana/spl-token';
import { BN } from '@project-serum/anchor';
import { usePerpsProgram } from './usePerpsProgram';
import {
  getUserAccountPDA,
  getCommodityMarket,
  getVaultPDA,
  getVaultTokenAccountPDA,
  getPositionPDA,
  getUserReferralPDA,
  isProgramAccount,
} from '../utils/pda';
import { parseAnchorError, getExplorerUrl } from '../utils/transaction';
import { useMarketStore } from '../stores/marketStore';
import {
  PERPS_CORE_PROGRAM_ID,
  USDC_MINT,
  PYTH_PRICE_FEEDS,
  PRICE_DECIMALS,
  LEVERAGE_DECIMALS,
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

        // Convert amount to 6 decimals
        const amountInUnits = new BN(amount * Math.pow(10, PRICE_DECIMALS));

        const signature = await program.methods
          .depositCollateral(amountInUnits)
          .accounts({
            owner: publicKey,
            userAccount: userAccountPda,
            market: marketPda,
            vault: vaultPda,
            vaultTokenAccount: vaultTokenAccountPda,
            userTokenAccount,
            tokenProgram: TOKEN_PROGRAM_ID,
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
        const errorMessage = parseAnchorError(error);
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

        // Convert amount to 6 decimals
        const amountInUnits = new BN(amount * Math.pow(10, PRICE_DECIMALS));

        const signature = await program.methods
          .withdrawCollateral(amountInUnits)
          .accounts({
            owner: publicKey,
            userAccount: userAccountPda,
            market: marketPda,
            vault: vaultPda,
            vaultTokenAccount: vaultTokenAccountPda,
            userTokenAccount,
            tokenProgram: TOKEN_PROGRAM_ID,
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
        const errorMessage = parseAnchorError(error);
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
   * Open a new position
   */
  const openPosition = useCallback(
    async (
      side: 'long' | 'short',
      size: number,
      leverage: number
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

        // Fetch market to get total_positions for position PDA
        const marketAccount = await program.account.market.fetch(marketPda);
        const positionIndex = (marketAccount.totalPositions as BN).toNumber();

        const [positionPda] = getPositionPDA(publicKey, marketPda, positionIndex);

        // Get Pyth price feed for selected commodity
        const pythPriceFeed =
          PYTH_PRICE_FEEDS[selectedCommodity.id] ||
          new PublicKey(selectedCommodity.pythPriceFeed);

        // Convert parameters
        const sideValue = side === 'long' ? 0 : 1;
        const sizeInUnits = new BN(size * Math.pow(10, PRICE_DECIMALS));
        const leverageWithDecimals = leverage * Math.pow(10, LEVERAGE_DECIMALS);

        console.log('Opening position:', {
          side,
          size,
          leverage,
          positionIndex,
          commodity: selectedCommodity.id,
        });

        const signature = await program.methods
          .openPosition({
            side: sideValue,
            size: sizeInUnits,
            leverage: leverageWithDecimals,
          })
          .accounts({
            owner: publicKey,
            userAccount: userAccountPda,
            market: marketPda,
            position: positionPda,
            pythPriceFeed,
            systemProgram: SystemProgram.programId,
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
        const errorMessage = parseAnchorError(error);
        console.error('Open position failed:', error);
        setState(prev => ({
          ...prev,
          isLoading: false,
          error: errorMessage,
        }));
        throw new Error(errorMessage);
      }
    },
    [program, publicKey, selectedCommodity, marketPda, initializeUserIfNeeded]
  );

  /**
   * Close an existing position
   */
  const closePosition = useCallback(
    async (positionAddress: string): Promise<string> => {
      if (!program || !publicKey) {
        throw new Error('Wallet not connected');
      }

      setState(prev => ({ ...prev, isLoading: true, error: null }));

      try {
        const positionPubkey = new PublicKey(positionAddress);

        // Fetch position to get market
        const positionAccount = await program.account.position.fetch(positionPubkey);
        const marketPda = positionAccount.market as PublicKey;

        // Fetch market for pyth feed and collateral mint
        const marketAccount = await program.account.market.fetch(marketPda);

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
          .closePosition()
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
        const errorMessage = parseAnchorError(error);
        console.error('Close position failed:', error);
        setState(prev => ({
          ...prev,
          isLoading: false,
          error: errorMessage,
        }));
        throw new Error(errorMessage);
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
    clearError,

    // Helpers
    getExplorerUrl: (signature: string) => getExplorerUrl(signature, 'testnet'),
  };
}
