import { useCallback, useState } from 'react';
import { useConnection } from '@solana/wallet-adapter-react';
import { PublicKey, SystemProgram } from '@solana/web3.js';
import { TOKEN_PROGRAM_ID, getAssociatedTokenAddress } from '@solana/spl-token';
import { BN } from '@project-serum/anchor';
import { usePerpsProgram } from './usePerpsProgram';
import {
  getUserAccountPDA,
  getMarketPDA,
  getVaultPDA,
  getVaultTokenAccountPDA,
  getPositionPDA,
} from '../utils/pda';
import { parseAnchorError, getExplorerUrl } from '../utils/transaction';
import { useMarketStore } from '../stores/marketStore';
import {
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

export function useTrading() {
  const { connection } = useConnection();
  const { program, publicKey, connected } = usePerpsProgram();
  const { selectedCommodity } = useMarketStore();

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

    const [userAccountPda] = getUserAccountPDA(publicKey);
    const accountInfo = await connection.getAccountInfo(userAccountPda);
    return accountInfo !== null;
  }, [publicKey, connection]);

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
    const [userAccountPda] = getUserAccountPDA(publicKey);

    await program.methods
      .initializeUser()
      .accounts({
        owner: publicKey,
        userAccount: userAccountPda,
        systemProgram: SystemProgram.programId,
      })
      .rpc();

    console.log('User account created');
  }, [program, publicKey, checkUserAccountExists]);

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

        const [userAccountPda] = getUserAccountPDA(publicKey);
        const [marketPda] = getMarketPDA(USDC_MINT);
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
    [program, publicKey, initializeUserIfNeeded]
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
        const [userAccountPda] = getUserAccountPDA(publicKey);
        const [marketPda] = getMarketPDA(USDC_MINT);
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
    [program, publicKey]
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
        const [userAccountPda] = getUserAccountPDA(publicKey);
        const [marketPda] = getMarketPDA(USDC_MINT);

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
    [program, publicKey, selectedCommodity, initializeUserIfNeeded]
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

        const [userAccountPda] = getUserAccountPDA(publicKey);
        const [vaultPda] = getVaultPDA(marketPda);
        const [vaultTokenAccountPda] = getVaultTokenAccountPDA(marketPda);

        // Get user's token account for receiving settlement
        const userTokenAccount = await getAssociatedTokenAddress(
          marketAccount.collateralMint as PublicKey,
          publicKey
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
    [program, publicKey]
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
