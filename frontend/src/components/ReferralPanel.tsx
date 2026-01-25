import { useState } from 'react';
import { useWallet } from '@solana/wallet-adapter-react';
import { useReferralStore } from '../stores/referralStore';

export function ReferralPanel() {
  const { publicKey, connected } = useWallet();
  const {
    myReferralCode,
    myReferralStats,
    appliedReferral,
    createReferralCode,
    applyReferralCode,
    claimRewards,
  } = useReferralStore();

  const [customCode, setCustomCode] = useState('');
  const [applyCode, setApplyCode] = useState('');
  const [applyError, setApplyError] = useState('');
  const [applySuccess, setApplySuccess] = useState(false);
  const [copied, setCopied] = useState(false);

  const handleCreateCode = () => {
    if (!publicKey) return;
    createReferralCode(publicKey.toBase58(), customCode || undefined);
    setCustomCode('');
  };

  const handleApplyCode = () => {
    if (!publicKey) return;
    setApplyError('');
    setApplySuccess(false);

    if (!applyCode.trim()) {
      setApplyError('Please enter a referral code');
      return;
    }

    const success = applyReferralCode(applyCode.trim(), publicKey.toBase58());
    if (success) {
      setApplySuccess(true);
      setApplyCode('');
    } else {
      setApplyError('Invalid code, already used, or cannot use your own code');
    }
  };

  const handleClaimRewards = () => {
    const claimed = claimRewards();
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
        <span className="referral-subtitle">Earn rewards by inviting traders</span>
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

      {/* My Referral Code Section */}
      <div className="referral-section">
        <h4>Your Referral Code</h4>
        {myReferralCode ? (
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
                onChange={(e) => setCustomCode(e.target.value.toUpperCase().slice(0, 12))}
                placeholder="Custom code (optional)"
                maxLength={12}
              />
              <button className="create-btn" onClick={handleCreateCode}>
                Create Code
              </button>
            </div>
          </div>
        )}
      </div>

      {/* Referral Stats Section */}
      {myReferralStats && (
        <div className="referral-section">
          <h4>Your Referral Stats</h4>
          <div className="stats-grid">
            <div className="stat-item">
              <span className="stat-value">{myReferralStats.totalReferred}</span>
              <span className="stat-label">Referred Users</span>
            </div>
            <div className="stat-item">
              <span className="stat-value">${(myReferralStats.totalVolume / 1_000_000).toFixed(2)}M</span>
              <span className="stat-label">Total Volume</span>
            </div>
            <div className="stat-item">
              <span className="stat-value">${myReferralStats.totalRewardsEarned.toFixed(2)}</span>
              <span className="stat-label">Total Earned</span>
            </div>
            <div className="stat-item highlight">
              <span className="stat-value">${myReferralStats.pendingRewards.toFixed(2)}</span>
              <span className="stat-label">Pending Rewards</span>
            </div>
          </div>

          {myReferralStats.pendingRewards > 0 && (
            <button className="claim-btn" onClick={handleClaimRewards}>
              Claim ${myReferralStats.pendingRewards.toFixed(2)} Rewards
            </button>
          )}

          {/* Referral Leaderboard */}
          {myReferralStats.referrals.length > 0 && (
            <div className="referrals-table">
              <h5>Your Referrals</h5>
              <table>
                <thead>
                  <tr>
                    <th>Wallet</th>
                    <th>Joined</th>
                    <th>Volume</th>
                    <th>Fees Generated</th>
                    <th>Your Rewards</th>
                  </tr>
                </thead>
                <tbody>
                  {myReferralStats.referrals.map((referral, index) => (
                    <tr key={index}>
                      <td className="wallet-cell">
                        {referral.walletAddress.slice(0, 4)}...{referral.walletAddress.slice(-4)}
                      </td>
                      <td>{new Date(referral.joinedAt).toLocaleDateString()}</td>
                      <td>${referral.totalVolume.toLocaleString()}</td>
                      <td>${referral.totalFeesGenerated.toFixed(2)}</td>
                      <td className="rewards-cell">${referral.rewardsEarned.toFixed(2)}</td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          )}
        </div>
      )}

      {/* Apply Referral Code Section */}
      <div className="referral-section">
        <h4>Use a Referral Code</h4>
        {appliedReferral ? (
          <div className="applied-code-display">
            <div className="applied-badge">
              <span className="checkmark">✓</span>
              Referral code applied
            </div>
            <div className="applied-info">
              <span>Code: <strong>{appliedReferral.code}</strong></span>
              <span className="discount-badge">
                {appliedReferral.discountPercent}% fee discount active
              </span>
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
                maxLength={12}
              />
              <button className="apply-btn" onClick={handleApplyCode}>
                Apply Code
              </button>
            </div>
            {applyError && <p className="error-message">{applyError}</p>}
            {applySuccess && <p className="success-message">Referral code applied successfully!</p>}
          </div>
        )}
      </div>

      {/* How it Works */}
      <div className="referral-section how-it-works">
        <h4>How It Works</h4>
        <div className="steps">
          <div className="step">
            <div className="step-number">1</div>
            <div className="step-content">
              <span className="step-title">Create Your Code</span>
              <span className="step-desc">Generate a unique referral code to share with friends</span>
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
              <span className="step-desc">Receive 20% of all trading fees from your referrals</span>
            </div>
          </div>
        </div>
      </div>
    </div>
  );
}
