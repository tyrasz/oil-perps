import { create } from 'zustand';
import { persist } from 'zustand/middleware';
import type { ReferralCode, ReferralStats, ReferralRecord, AppliedReferral } from '../types';

interface ReferralState {
  // User's own referral code (if they created one)
  myReferralCode: ReferralCode | null;

  // Stats for user's referral code
  myReferralStats: ReferralStats | null;

  // Referral code applied to this user (if they used someone's code)
  appliedReferral: AppliedReferral | null;

  // Actions
  createReferralCode: (walletAddress: string, customCode?: string) => ReferralCode;
  applyReferralCode: (code: string, userWallet: string) => boolean;
  recordReferralVolume: (walletAddress: string, volume: number, fees: number) => void;
  claimRewards: () => number;
  getDiscountedFee: (baseFee: number) => number;

  // Internal
  allReferralCodes: Record<string, ReferralCode>;
  allReferralStats: Record<string, ReferralStats>;
}

// Generate a random referral code
const generateCode = (): string => {
  const chars = 'ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789';
  let code = '';
  for (let i = 0; i < 8; i++) {
    code += chars.charAt(Math.floor(Math.random() * chars.length));
  }
  return code;
};

// Default referral parameters
const DEFAULT_DISCOUNT_PERCENT = 10; // 10% fee discount for referred users
const DEFAULT_REWARD_PERCENT = 20;   // 20% of fees go to referrer

export const useReferralStore = create<ReferralState>()(
  persist(
    (set, get) => ({
      myReferralCode: null,
      myReferralStats: null,
      appliedReferral: null,
      allReferralCodes: {},
      allReferralStats: {},

      createReferralCode: (walletAddress: string, customCode?: string) => {
        const state = get();

        // Check if user already has a referral code
        if (state.myReferralCode) {
          return state.myReferralCode;
        }

        // Generate or use custom code
        let code = customCode?.toUpperCase() || generateCode();

        // Ensure code is unique
        while (state.allReferralCodes[code]) {
          code = generateCode();
        }

        const newReferralCode: ReferralCode = {
          code,
          ownerWallet: walletAddress,
          createdAt: Date.now(),
          discountPercent: DEFAULT_DISCOUNT_PERCENT,
          rewardPercent: DEFAULT_REWARD_PERCENT,
        };

        const newStats: ReferralStats = {
          code,
          totalReferred: 0,
          totalVolume: 0,
          totalRewardsEarned: 0,
          pendingRewards: 0,
          referrals: [],
        };

        set({
          myReferralCode: newReferralCode,
          myReferralStats: newStats,
          allReferralCodes: {
            ...state.allReferralCodes,
            [code]: newReferralCode,
          },
          allReferralStats: {
            ...state.allReferralStats,
            [code]: newStats,
          },
        });

        return newReferralCode;
      },

      applyReferralCode: (code: string, userWallet: string) => {
        const state = get();
        const upperCode = code.toUpperCase();

        // Check if code exists
        const referralCode = state.allReferralCodes[upperCode];
        if (!referralCode) {
          return false;
        }

        // Can't use your own referral code
        if (referralCode.ownerWallet === userWallet) {
          return false;
        }

        // Check if user already has an applied referral
        if (state.appliedReferral) {
          return false;
        }

        const applied: AppliedReferral = {
          code: upperCode,
          referrerWallet: referralCode.ownerWallet,
          appliedAt: Date.now(),
          discountPercent: referralCode.discountPercent,
        };

        // Update referrer's stats
        const currentStats = state.allReferralStats[upperCode];
        const newReferral: ReferralRecord = {
          walletAddress: userWallet,
          joinedAt: Date.now(),
          totalVolume: 0,
          totalFeesGenerated: 0,
          rewardsEarned: 0,
        };

        const updatedStats: ReferralStats = {
          ...currentStats,
          totalReferred: currentStats.totalReferred + 1,
          referrals: [...currentStats.referrals, newReferral],
        };

        set({
          appliedReferral: applied,
          allReferralStats: {
            ...state.allReferralStats,
            [upperCode]: updatedStats,
          },
          // Update myReferralStats if this is my code
          myReferralStats: state.myReferralCode?.code === upperCode
            ? updatedStats
            : state.myReferralStats,
        });

        return true;
      },

      recordReferralVolume: (walletAddress: string, volume: number, fees: number) => {
        const state = get();

        // Find which referral code this wallet is using
        let referralCode: string | null = null;

        for (const [code, stats] of Object.entries(state.allReferralStats)) {
          if (stats.referrals.some(r => r.walletAddress === walletAddress)) {
            referralCode = code;
            break;
          }
        }

        if (!referralCode) return;

        const codeInfo = state.allReferralCodes[referralCode];
        const currentStats = state.allReferralStats[referralCode];
        const rewardAmount = fees * (codeInfo.rewardPercent / 100);

        // Update the referral record
        const updatedReferrals = currentStats.referrals.map(r => {
          if (r.walletAddress === walletAddress) {
            return {
              ...r,
              totalVolume: r.totalVolume + volume,
              totalFeesGenerated: r.totalFeesGenerated + fees,
              rewardsEarned: r.rewardsEarned + rewardAmount,
            };
          }
          return r;
        });

        const updatedStats: ReferralStats = {
          ...currentStats,
          totalVolume: currentStats.totalVolume + volume,
          totalRewardsEarned: currentStats.totalRewardsEarned + rewardAmount,
          pendingRewards: currentStats.pendingRewards + rewardAmount,
          referrals: updatedReferrals,
        };

        set({
          allReferralStats: {
            ...state.allReferralStats,
            [referralCode]: updatedStats,
          },
          myReferralStats: state.myReferralCode?.code === referralCode
            ? updatedStats
            : state.myReferralStats,
        });
      },

      claimRewards: () => {
        const state = get();

        if (!state.myReferralCode || !state.myReferralStats) {
          return 0;
        }

        const pendingAmount = state.myReferralStats.pendingRewards;

        if (pendingAmount <= 0) {
          return 0;
        }

        const updatedStats: ReferralStats = {
          ...state.myReferralStats,
          pendingRewards: 0,
        };

        set({
          myReferralStats: updatedStats,
          allReferralStats: {
            ...state.allReferralStats,
            [state.myReferralCode.code]: updatedStats,
          },
        });

        // In production, this would trigger an on-chain transfer
        console.log(`Claimed ${pendingAmount} in referral rewards`);
        return pendingAmount;
      },

      getDiscountedFee: (baseFee: number) => {
        const state = get();

        if (!state.appliedReferral) {
          return baseFee;
        }

        const discount = baseFee * (state.appliedReferral.discountPercent / 100);
        return baseFee - discount;
      },
    }),
    {
      name: 'oil-perps-referral-storage',
    }
  )
);
