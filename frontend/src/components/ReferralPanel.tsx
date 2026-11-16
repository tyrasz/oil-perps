import { useState } from 'react';
import { useWallet } from '@solana/wallet-adapter-react';
import { useOnChainReferral } from '../hooks/useOnChainReferral';

export function ReferralPanel() {
  const { publicKey, connected } = useWallet();
  const {
    myReferralCode,
    userReferral,
    isLoading,
    error,
    createReferralCode,
    applyReferralCode,
    claimRewards,
  } = useOnChainReferral();

  const [customCode, setCustomCode] = useState('');
  const [applyCode, setApplyCode] = useState('');
  const [applyError, setApplyError] = useState('');
  const [applySuccess, setApplySuccess] = useState(false);
  const [copied, setCopied] = useState(false);
  const [isSubmitting, setIsSubmitting] = useState(false);

  const handleCreateCode = async () => {
    if (!publicKey || isSubmitting) return;

    // Validate code format
    const code = customCode.trim().toUpperCase() || generateRandomCode();
    if (code.length < 4) {
      setApplyError('Code must be at least 4 characters');
      return;
    }
    if (!/^[A-Z0-9]+$/.test(code)) {
      setApplyError('Code must be alphanumeric only');
      return;
    }

    setIsSubmitting(true);
    const success = await createReferralCode(code);
    setIsSubmitting(false);

    if (success) {
      setCustomCode('');
    }
  };

  const handleApplyCode = async () => {
    if (!publicKey || isSubmitting) return;
    setApplyError('');
    setApplySuccess(false);

    if (!applyCode.trim()) {
      setApplyError('Please enter a referral code');
      return;
    }

    setIsSubmitting(true);
    const success = await applyReferralCode(applyCode.trim());
    setIsSubmitting(false);

    if (success) {
      setApplySuccess(true);
      setApplyCode('');
    } else {
      setApplyError(error || 'Failed to apply referral code');
    }
  };

  const handleClaimRewards = async () => {
    if (isSubmitting) return;
    setIsSubmitting(true);
    const claimed = await claimRewards();
    setIsSubmitting(false);

    if (claimed > 0) {
      alert(`Successfully claimed $${claimed.toFixed(2)} in referral rewards!`);
    }
  };

  const copyToClipboard = (text: string) => {
    navigator.clipboard.writeText(text);
    setCopied(true);
    setTimeout(() => setCopied(false), 2000);
  };

  const getReferralLink = (code: string) => {
    return `${window.location.origin}?ref=${code}`;
  };

  // Generate random 8-character code
  const generateRandomCode = (): string => {
    const chars = 'ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789';
    let code = '';
    for (let i = 0; i < 8; i++) {
      code += chars.charAt(Math.floor(Math.random() * chars.length));
    }
    return code;
  };

  if (!connected) {
    return (
      <div className="referral-panel">
        <div className="referral-header">
          <h3>Referral Program</h3>
        </div>
        <div className="referral-notice">
          Connect your wallet to access the referral program.
        </div>
      </div>
    );
  }

  return (
    <div className="referral-panel">
      <div className="referral-header">
        <h3>Referral Program</h3>
        <span className="referral-subtitle">Earn rewards by inviting traders (On-Chain)</span>
      </div>

      {/* On-chain indicator */}
      <div className="onchain-badge">
        <span className="badge-dot" />
        Secured On-Chain
      </div>

      {/* Benefits Overview */}
      <div className="referral-benefits">
        <div className="benefit-card">
          <div className="benefit-icon">💰</div>
          <div className="benefit-content">
            <span className="benefit-value">20%</span>
            <span className="benefit-label">of referred fees</span>
          </div>
        </div>
        <div className="benefit-card">
          <div className="benefit-icon">🎁</div>
          <div className="benefit-content">
            <span className="benefit-value">10%</span>
            <span className="benefit-label">fee discount for friends</span>
          </div>
        </div>
      </div>

      {/* Global error display */}
      {error && !applyError && (
        <div className="error-message">{error}</div>
      )}

      {/* My Referral Code Section */}
      <div className="referral-section">
        <h4>Your Referral Code</h4>
        {isLoading && !myReferralCode ? (
          <div className="loading">Loading...</div>
        ) : myReferralCode ? (
          <div className="my-code-display">
            <div className="code-box">
              <span className="code-value">{myReferralCode.code}</span>
              <button
                className="copy-btn"
                onClick={() => copyToClipboard(myReferralCode.code)}
              >
                {copied ? 'Copied!' : 'Copy'}
              </button>
            </div>
            <div className="share-link">
              <input
                type="text"
                value={getReferralLink(myReferralCode.code)}
                readOnly
              />
              <button
                className="copy-btn"
                onClick={() => copyToClipboard(getReferralLink(myReferralCode.code))}
              >
                Copy Link
              </button>
            </div>
          </div>
        ) : (
          <div className="create-code-section">
            <p>Create your referral code to start earning rewards.</p>
            <div className="create-code-form">
              <input
                type="text"
                value={customCode}
                onChange={(e) => setCustomCode(e.target.value.toUpperCase().slice(0, 8))}
                placeholder="Custom code (optional)"
                maxLength={8}
                disabled={isSubmitting}
              />
              <button
                className="create-btn"
                onClick={handleCreateCode}
                disabled={isSubmitting}
              >
                {isSubmitting ? 'Creating...' : 'Create Code'}
              </button>
            </div>
            <p className="hint">Leave empty for a random code. Must be 4-8 alphanumeric characters.</p>
          </div>
        )}
      </div>

      {/* Referral Stats Section */}
      {myReferralCode && (
        <div className="referral-section">
          <h4>Your Referral Stats</h4>
          <div className="stats-grid">
            <div className="stat-item">
              <span className="stat-value">{myReferralCode.totalReferred}</span>
              <span className="stat-label">Referred Users</span>
            </div>
            <div className="stat-item">
              <span className="stat-value">${myReferralCode.totalVolume.toLocaleString()}</span>
              <span className="stat-label">Total Volume</span>
            </div>
            <div className="stat-item">
              <span className="stat-value">${myReferralCode.totalRewardsEarned.toFixed(2)}</span>
              <span className="stat-label">Total Earned</span>
            </div>
            <div className="stat-item highlight">
              <span className="stat-value">${myReferralCode.pendingRewards.toFixed(2)}</span>
              <span className="stat-label">Pending Rewards</span>
            </div>
          </div>

          {myReferralCode.pendingRewards > 0 && (
            <button
              className="claim-btn"
              onClick={handleClaimRewards}
              disabled={isSubmitting}
            >
              {isSubmitting ? 'Claiming...' : `Claim $${myReferralCode.pendingRewards.toFixed(2)} Rewards`}
            </button>
          )}
        </div>
      )}

      {/* Apply Referral Code Section */}
      <div className="referral-section">
        <h4>Use a Referral Code</h4>
        {userReferral ? (
          <div className="applied-code-display">
            <div className="applied-badge">
              <span className="checkmark">✓</span>
              Referral code applied
            </div>
            <div className="applied-info">
              <span>Referrer: <strong>{userReferral.referrer.slice(0, 4)}...{userReferral.referrer.slice(-4)}</strong></span>
              <span className="discount-badge">
                {(userReferral.discountBps / 100).toFixed(0)}% fee discount active
              </span>
            </div>
            <div className="referral-stats-mini">
              <span>Your volume: ${userReferral.totalVolume.toLocaleString()}</span>
              <span>Fees saved: ${(userReferral.totalFeesPaid * userReferral.discountBps / 10000).toFixed(2)}</span>
            </div>
          </div>
        ) : (
          <div className="apply-code-section">
            <p>Enter a friend's referral code to get a 10% discount on trading fees.</p>
            <div className="apply-code-form">
              <input
                type="text"
                value={applyCode}
                onChange={(e) => {
                  setApplyCode(e.target.value.toUpperCase());
                  setApplyError('');
                  setApplySuccess(false);
                }}
                placeholder="Enter referral code"
                maxLength={8}
                disabled={isSubmitting}
              />
              <button
                className="apply-btn"
                onClick={handleApplyCode}
                disabled={isSubmitting}
              >
                {isSubmitting ? 'Applying...' : 'Apply Code'}
              </button>
            </div>
            {applyError && <p className="error-message">{applyError}</p>}
            {applySuccess && <p className="success-message">Referral code applied successfully!</p>}
          </div>
        )}
      </div>

      {/* Security Notice */}
      <div className="referral-section security-notice">
        <h4>Security</h4>
        <ul>
          <li>All referral data is stored on-chain in Solana accounts</li>
          <li>Rewards are calculated from actual trading fees</li>
          <li>Self-referral is prevented at the program level</li>
          <li>Claims transfer real USDC from the vault</li>
        </ul>
      </div>

      {/* How it Works */}
      <div className="referral-section how-it-works">
        <h4>How It Works</h4>
        <div className="steps">
          <div className="step">
            <div className="step-number">1</div>
            <div className="step-content">
              <span className="step-title">Create Your Code</span>
              <span className="step-desc">Generate a unique referral code stored on Solana</span>
            </div>
          </div>
          <div className="step">
            <div className="step-number">2</div>
            <div className="step-content">
              <span className="step-title">Share & Invite</span>
              <span className="step-desc">Friends sign up using your code and get 10% off fees</span>
            </div>
          </div>
          <div className="step">
            <div className="step-number">3</div>
            <div className="step-content">
              <span className="step-title">Earn Rewards</span>
              <span className="step-desc">Receive 20% of trading fees, tracked on every trade</span>
            </div>
          </div>
        </div>
      </div>
    </div>
  );
}
