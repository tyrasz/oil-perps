import { useCallback, useEffect, useState } from 'react';
import { usePerpsProgram } from './usePerpsProgram';
import { getReferralCodePDA, getUserReferralPDA, codeToBytes, bytesToCode } from '../utils/pda';
import { USDC_MINT, PRICE_DECIMALS } from '../config/program';
import { getMarketPDA, getVaultPDA, getVaultTokenAccountPDA } from '../utils/pda';

// On-chain referral code data
interface OnChainReferralCode {
  address: string;
  owner: string;
  code: string;
  discountBps: number;
  rewardBps: number;
  totalReferred: number;
  totalVolume: number;
  totalFeesGenerated: number;
  totalRewardsEarned: number;
  pendingRewards: number;
  createdAt: number;
  isActive: boolean;
}

// On-chain user referral data
interface OnChainUserReferral {
  address: string;
  user: string;
  referralCode: string;
  referrer: string;
  discountBps: number;
  totalVolume: number;
  totalFeesPaid: number;
  totalReferrerRewards: number;
  appliedAt: number;
}

interface ReferralAccount {
  owner: { toString: () => string };
  code: number[];
  discountBps: number;
  rewardBps: number;
  totalReferred: number;
  totalVolume: { toNumber: () => number };
  totalFeesGenerated: { toNumber: () => number };
  totalRewardsEarned: { toNumber: () => number };
  pendingRewards: { toNumber: () => number };
  createdAt: { toNumber: () => number };
  isActive: boolean;
}

interface UserReferralAccount {
  user: { toString: () => string };
  referralCode: { toString: () => string };
  referrer: { toString: () => string };
  discountBps: number;
  totalVolume: { toNumber: () => number };
  totalFeesPaid: { toNumber: () => number };
  totalReferrerRewards: { toNumber: () => number };
  appliedAt: { toNumber: () => number };
}

export function useOnChainReferral() {
  const { program, publicKey, connected } = usePerpsProgram();
  const [myReferralCode, setMyReferralCode] = useState<OnChainReferralCode | null>(null);
  const [userReferral, setUserReferral] = useState<OnChainUserReferral | null>(null);
  const [isLoading, setIsLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Fetch user's referral code (if they created one)
  const fetchMyReferralCode = useCallback(async () => {
    if (!program || !publicKey) return null;

    try {
      // We need to search for referral codes owned by this user
      // This is a limitation - in production you'd use an indexer
      // For now, we'll check if user has created a code by looking at account filters
      const accounts = await program.account.referralCode.all([
        {
          memcmp: {
            offset: 8, // Skip discriminator
            bytes: publicKey.toBase58(),
          },
        },
      ]);

      if (accounts.length > 0) {
        const account = accounts[0];
        const data = account.account as unknown as ReferralAccount;
        return {
          address: account.publicKey.toString(),
          owner: data.owner.toString(),
          code: bytesToCode(data.code),
          discountBps: data.discountBps,
          rewardBps: data.rewardBps,
          totalReferred: data.totalReferred,
          totalVolume: data.totalVolume.toNumber() / Math.pow(10, PRICE_DECIMALS),
          totalFeesGenerated: data.totalFeesGenerated.toNumber() / Math.pow(10, PRICE_DECIMALS),
          totalRewardsEarned: data.totalRewardsEarned.toNumber() / Math.pow(10, PRICE_DECIMALS),
          pendingRewards: data.pendingRewards.toNumber() / Math.pow(10, PRICE_DECIMALS),
          createdAt: data.createdAt.toNumber() * 1000,
          isActive: data.isActive,
        };
      }
      return null;
    } catch {
      return null;
    }
  }, [program, publicKey]);

  // Fetch user's applied referral
  const fetchUserReferral = useCallback(async () => {
    if (!program || !publicKey) return null;

    try {
      const [userReferralPda] = getUserReferralPDA(publicKey);
      const account = await program.account.userReferral.fetch(userReferralPda) as unknown as UserReferralAccount;

      return {
        address: userReferralPda.toString(),
        user: account.user.toString(),
        referralCode: account.referralCode.toString(),
        referrer: account.referrer.toString(),
        discountBps: account.discountBps,
        totalVolume: account.totalVolume.toNumber() / Math.pow(10, PRICE_DECIMALS),
        totalFeesPaid: account.totalFeesPaid.toNumber() / Math.pow(10, PRICE_DECIMALS),
        totalReferrerRewards: account.totalReferrerRewards.toNumber() / Math.pow(10, PRICE_DECIMALS),
        appliedAt: account.appliedAt.toNumber() * 1000,
      };
    } catch {
      return null;
    }
  }, [program, publicKey]);

  // Refresh all data
  const refresh = useCallback(async () => {
    if (!connected) {
      setMyReferralCode(null);
      setUserReferral(null);
      return;
    }

    setIsLoading(true);
    setError(null);

    try {
      const [code, referral] = await Promise.all([
        fetchMyReferralCode(),
        fetchUserReferral(),
      ]);

      setMyReferralCode(code);
      setUserReferral(referral);
    } catch (err) {
      console.error('Failed to fetch referral data:', err);
      setError('Failed to load referral data');
    } finally {
      setIsLoading(false);
    }
  }, [connected, fetchMyReferralCode, fetchUserReferral]);

  // Create a new referral code
  const createReferralCode = useCallback(async (code: string): Promise<boolean> => {
    if (!program || !publicKey) return false;

    try {
      setIsLoading(true);
      setError(null);

      const codeBytes = codeToBytes(code);
      const [referralCodePda] = getReferralCodePDA(code);

      await program.methods
        .createReferralCode({ code: codeBytes })
        .accounts({
          owner: publicKey,
          referralCode: referralCodePda,
        })
        .rpc();

      await refresh();
      return true;
    } catch (err: unknown) {
      console.error('Failed to create referral code:', err);
      const errorMessage = err instanceof Error ? err.message : 'Unknown error';
      if (errorMessage.includes('already in use')) {
        setError('This referral code already exists');
      } else {
        setError('Failed to create referral code');
      }
      return false;
    } finally {
      setIsLoading(false);
    }
  }, [program, publicKey, refresh]);

  // Apply a referral code
  const applyReferralCode = useCallback(async (code: string): Promise<boolean> => {
    if (!program || !publicKey) return false;

    try {
      setIsLoading(true);
      setError(null);

      const [referralCodePda] = getReferralCodePDA(code);
      const [userReferralPda] = getUserReferralPDA(publicKey);

      await program.methods
        .applyReferralCode()
        .accounts({
          user: publicKey,
          referralCode: referralCodePda,
          userReferral: userReferralPda,
        })
        .rpc();

      await refresh();
      return true;
    } catch (err: unknown) {
      console.error('Failed to apply referral code:', err);
      const errorMessage = err instanceof Error ? err.message : 'Unknown error';
      if (errorMessage.includes('SelfReferralNotAllowed')) {
        setError('Cannot use your own referral code');
      } else if (errorMessage.includes('already in use')) {
        setError('You already have a referral applied');
      } else if (errorMessage.includes('not found')) {
        setError('Referral code not found');
      } else {
        setError('Failed to apply referral code');
      }
      return false;
    } finally {
      setIsLoading(false);
    }
  }, [program, publicKey, refresh]);

  // Claim pending rewards
  const claimRewards = useCallback(async (): Promise<number> => {
    if (!program || !publicKey || !myReferralCode) return 0;

    try {
      setIsLoading(true);
      setError(null);

      const [referralCodePda] = getReferralCodePDA(myReferralCode.code);
      const [marketPda] = getMarketPDA(USDC_MINT);
      const [vaultPda] = getVaultPDA(marketPda);
      const [vaultTokenPda] = getVaultTokenAccountPDA(marketPda);

      // Get user's token account
      const { getAssociatedTokenAddress } = await import('@solana/spl-token');
      const ownerTokenAccount = await getAssociatedTokenAddress(USDC_MINT, publicKey);

      const pendingAmount = myReferralCode.pendingRewards;

      await program.methods
        .claimReferralRewards()
        .accounts({
          owner: publicKey,
          referralCode: referralCodePda,
          market: marketPda,
          vault: vaultPda,
          vaultTokenAccount: vaultTokenPda,
          ownerTokenAccount: ownerTokenAccount,
        })
        .rpc();

      await refresh();
      return pendingAmount;
    } catch (err) {
      console.error('Failed to claim rewards:', err);
      setError('Failed to claim rewards');
      return 0;
    } finally {
      setIsLoading(false);
    }
  }, [program, publicKey, myReferralCode, refresh]);

  // Check if a code exists
  const checkCodeExists = useCallback(async (code: string): Promise<boolean> => {
    if (!program) return false;

    try {
      const [referralCodePda] = getReferralCodePDA(code);
      await program.account.referralCode.fetch(referralCodePda);
      return true;
    } catch {
      return false;
    }
  }, [program]);

  // Get fee discount for current user
  const getDiscountBps = useCallback((): number => {
    return userReferral?.discountBps || 0;
  }, [userReferral]);

  // Load data on mount and when wallet changes
  useEffect(() => {
    refresh();
  }, [refresh]);

  return {
    myReferralCode,
    userReferral,
    isLoading,
    error,
    createReferralCode,
    applyReferralCode,
    claimRewards,
    checkCodeExists,
    getDiscountBps,
    refresh,
  };
}
