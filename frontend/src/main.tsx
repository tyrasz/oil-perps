import { StrictMode, useMemo } from 'react';
import { createRoot } from 'react-dom/client';
import { ConnectionProvider, WalletProvider } from '@solana/wallet-adapter-react';
import { WalletModalProvider } from '@solana/wallet-adapter-react-ui';
import { PhantomWalletAdapter, SolflareWalletAdapter, UnsafeBurnerWalletAdapter } from '@solana/wallet-adapter-wallets';
import { clusterApiUrl } from '@solana/web3.js';
import App from './App';
import { NETWORK, RPC_ENDPOINTS } from './config/program';
import './index.css';
import '@solana/wallet-adapter-react-ui/styles.css';

function Root() {
  const endpoint = useMemo(() =>
    import.meta.env.VITE_RPC_URL || (NETWORK === 'localnet' ? RPC_ENDPOINTS.localnet : clusterApiUrl('devnet')),
    []
  );

  const wallets = useMemo(() => [
    // Localnet only (docs/localnet.md): an in-browser throwaway key, so the
    // live local simulation can be viewed without a browser wallet
    ...(NETWORK === 'localnet' ? [new UnsafeBurnerWalletAdapter()] : []),
    new PhantomWalletAdapter(),
    new SolflareWalletAdapter(),
  ], []);

  return (
    <ConnectionProvider endpoint={endpoint}>
      <WalletProvider wallets={wallets} autoConnect>
        <WalletModalProvider>
          <App />
        </WalletModalProvider>
      </WalletProvider>
    </ConnectionProvider>
  );
}

createRoot(document.getElementById('root')!).render(
  <StrictMode>
    <Root />
  </StrictMode>
);
