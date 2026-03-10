/**
 * Kabosu Browser Wallet Integration
 *
 * Supports: MyDoge (window.doge), Dojak (window.dojak), Nintondo (window.nintondo)
 * and a basic Browser Wallet (WIF key stored in localStorage).
 *
 * Architecture mirrors borkstarter/frontend/src/wallets — each wallet type is an
 * adapter implementing the same interface so callers are wallet-agnostic.
 */

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const WALLET_TYPES = { MYDOGE: 'mydoge', DOJAK: 'dojak', NINTONDO: 'nintondo', BROWSER: 'browser' };
const LS_KEY_TYPE    = 'kabosu_wallet_type';
const LS_KEY_ADDR    = 'kabosu_wallet_address';
const LS_KEY_BROWSER = 'kabosu_browser_wallet';

// Dogecoin mainnet params
const DOGE_NETWORK_PARAMS = {
    pubKeyHash:  0x1e,
    scriptHash:  0x16,
    wif:         0x9e,
    bip32Public: 0x02FACAFD,
    bip32Private: 0x02FAC398,
};

// ---------------------------------------------------------------------------
// Base58Check helper (no external deps, used for WIF address display)
// ---------------------------------------------------------------------------

const BASE58_ALPHABET = '123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz';

function base58Decode(s) {
    let n = BigInt(0);
    for (const c of s) {
        const idx = BASE58_ALPHABET.indexOf(c);
        if (idx < 0) throw new Error(`Invalid base58 char: ${c}`);
        n = n * 64n + BigInt(idx);  // will correct below
    }
    // Proper base58 decode
    n = BigInt(0);
    for (const c of s) n = n * 58n + BigInt(BASE58_ALPHABET.indexOf(c));
    const hex = n.toString(16).padStart(2, '0');
    const bytes = hex.match(/.{1,2}/g).map(b => parseInt(b, 16));
    const leading = s.match(/^1*/)[0].length;
    return new Uint8Array([...new Array(leading).fill(0), ...bytes]);
}

/**
 * Derive a Dogecoin P2PKH address from a WIF private key.
 * This is a display-only helper — no signing is done here.
 */
async function addressFromWIF(wif) {
    try {
        const decoded = base58Decode(wif);
        // WIF: 1 byte prefix + 32 bytes key [+ 0x01 compression flag] + 4 byte checksum
        const compressed = decoded.length === 38;
        const rawKey = decoded.slice(1, 33);
        // Derive public key via SubtleCrypto (P-256 is not secp256k1, so we can't use SubtleCrypto here)
        // We store the WIF and display it — actual signing requires an extension or external lib.
        return null; // Signing deferred to extension wallets; browser wallet address shown after user entry
    } catch (_) {
        return null;
    }
}

// ---------------------------------------------------------------------------
// Wallet Adapters
// ---------------------------------------------------------------------------

class MyDogeAdapter {
    get name()  { return 'MyDoge'; }
    get icon()  { return '🐕'; }
    get type()  { return WALLET_TYPES.MYDOGE; }

    isAvailable() {
        return !!(window.doge && window.doge.isMyDoge);
    }

    async connect() {
        const result = await window.doge.connect();
        if (!result.approved) throw new Error('MyDoge connection rejected');
        return { address: result.address };
    }

    async disconnect() {
        await window.doge.disconnect().catch(() => {});
    }

    async isConnected() {
        try { return (await window.doge.getConnectionStatus()).connected; }
        catch (_) { return false; }
    }

    async getAddress() {
        const status = await window.doge.getConnectionStatus();
        if (!status.connected) return null;
        if (window.doge.getCurrentAddress) {
            const r = await window.doge.getCurrentAddress().catch(() => null);
            if (r?.address) return r.address;
        }
        return null;
    }

    async getBalance() {
        const r = await window.doge.getBalance();
        return parseFloat(r.balance) / 1e8;
    }

    async signMessage(message) {
        const r = await window.doge.requestSignedMessage({ message });
        return r.signature;
    }

    async signPSBT(psbtHex) {
        const r = await window.doge.signPSBT({ psbtHex });
        return r.signedRawTx;
    }

    async sendTransaction(recipientAddress, dogeAmount) {
        const r = await window.doge.requestTransaction({ recipientAddress, dogeAmount: String(dogeAmount) });
        return r.txId;
    }
}

class DojakAdapter {
    get name()  { return 'Dojak'; }
    get icon()  { return '🎯'; }
    get type()  { return WALLET_TYPES.DOJAK; }

    isAvailable() {
        return !!(window.dojak && window.dojak.isDojak);
    }

    async connect() {
        await window.dojak.request({ method: 'dojak_switchChain', params: ['dogecoin-mainnet'] }).catch(() => {});
        const accounts = await window.dojak.requestAccounts();
        if (!accounts || accounts.length === 0) throw new Error('Dojak returned no accounts');
        return { address: accounts[0] };
    }

    async disconnect() {
        // Dojak has no explicit disconnect RPC
    }

    async isConnected() {
        try {
            const accounts = await window.dojak.getAccounts();
            return accounts && accounts.length > 0;
        } catch (_) { return false; }
    }

    async getAddress() {
        const accounts = await window.dojak.getAccounts();
        return accounts?.[0] ?? null;
    }

    async getBalance() {
        const r = await window.dojak.getBalance();
        const raw = r.total ?? r.confirmed ?? 0;
        return parseFloat(raw) / 1e8;
    }

    async signMessage(message) {
        return window.dojak.signMessage(message);
    }

    async signPSBT(psbtHex) {
        return window.dojak.signPsbt(psbtHex);
    }

    async getDRC20Balance(ticker) {
        return window.dojak.getDRC20Balance(ticker);
    }

    async sendInscription(toAddress, inscriptionId) {
        return window.dojak.request({ method: 'dojak_sendInscription', params: [toAddress, inscriptionId] });
    }
}

class NintondoAdapter {
    get name()  { return 'Nintondo'; }
    get icon()  { return '🎮'; }
    get type()  { return WALLET_TYPES.NINTONDO; }

    isAvailable() {
        return !!(window.nintondo && typeof window.nintondo.connect === 'function');
    }

    async connect() {
        await new Promise(r => setTimeout(r, 500)); // allow extension to init
        const address = await window.nintondo.connect();
        if (!address) throw new Error('Nintondo returned no address');
        return { address };
    }

    async disconnect() {}

    async isConnected() {
        try { return await window.nintondo.isConnected(); }
        catch (_) { return false; }
    }

    async getAddress() {
        return window.nintondo.getAccount().catch(() => null);
    }

    async getBalance() {
        const r = await window.nintondo.getBalance();
        return parseFloat(r) / 1e8;
    }

    async signMessage(message) {
        return window.nintondo.signMessage(message);
    }

    async signPSBT(psbtHex) {
        // Nintondo expects base64
        const psbtBase64 = btoa(psbtHex.match(/.{1,2}/g).map(h => String.fromCharCode(parseInt(h, 16))).join(''));
        return window.nintondo.signPsbt(psbtBase64);
    }
}

/**
 * Browser Wallet — stores a WIF private key in localStorage.
 * Signing is NOT yet implemented (requires secp256k1 — add doge-sdk via CDN when needed).
 * The adapter is fully wired so it can be upgraded in place.
 */
class BrowserWalletAdapter {
    get name()  { return 'Browser Wallet'; }
    get icon()  { return '🌐'; }
    get type()  { return WALLET_TYPES.BROWSER; }

    isAvailable() { return true; } // always available

    _load() {
        try { return JSON.parse(localStorage.getItem(LS_KEY_BROWSER) || 'null'); } catch (_) { return null; }
    }

    _save(data) { localStorage.setItem(LS_KEY_BROWSER, JSON.stringify(data)); }

    /** Import an existing wallet from a WIF private key (no signing yet). */
    importWIF(wif, address) {
        this._save({ wif, address });
    }

    removeWallet() {
        localStorage.removeItem(LS_KEY_BROWSER);
    }

    hasWallet() { return !!this._load(); }

    async connect() {
        const data = this._load();
        if (!data?.address) throw new Error('No browser wallet configured. Import a WIF key first.');
        return { address: data.address };
    }

    async disconnect() {}

    async isConnected() { return !!this._load()?.address; }

    async getAddress() { return this._load()?.address ?? null; }

    async getBalance() {
        const addr = await this.getAddress();
        if (!addr) return 0;
        try {
            const r = await fetch(`https://dogechain.info/api/v1/address/balance/${addr}`);
            const j = await r.json();
            return parseFloat(j.balance?.balance ?? 0);
        } catch (_) { return 0; }
    }

    async signMessage(_message) {
        throw new Error('Browser wallet signing not yet supported. Use MyDoge or Dojak extension.');
    }

    async signPSBT(_psbtHex) {
        throw new Error('Browser wallet PSBT signing not yet supported. Use MyDoge or Dojak extension.');
    }
}

// ---------------------------------------------------------------------------
// WalletManager — single active connection, persists across page loads
// ---------------------------------------------------------------------------

class WalletManager {
    constructor() {
        this.adapters = {
            [WALLET_TYPES.MYDOGE]:   new MyDogeAdapter(),
            [WALLET_TYPES.DOJAK]:    new DojakAdapter(),
            [WALLET_TYPES.NINTONDO]: new NintondoAdapter(),
            [WALLET_TYPES.BROWSER]:  new BrowserWalletAdapter(),
        };
        this.activeType    = null;
        this.activeAddress = null;
        this.activeBalance = 0;
        this._listeners    = [];
    }

    get active() { return this.adapters[this.activeType] ?? null; }

    on(cb) { this._listeners.push(cb); }
    _emit(event, data) { this._listeners.forEach(fn => fn(event, data)); }

    async connect(type) {
        if (this.activeType) await this.disconnect();
        const adapter = this.adapters[type];
        if (!adapter) throw new Error(`Unknown wallet type: ${type}`);
        const { address } = await adapter.connect();
        this.activeType    = type;
        this.activeAddress = address;
        localStorage.setItem(LS_KEY_TYPE, type);
        localStorage.setItem(LS_KEY_ADDR, address);
        this._emit('connect', { type, address });
        this.refreshBalance().catch(() => {});
        return address;
    }

    async disconnect() {
        if (this.active) await this.active.disconnect().catch(() => {});
        this.activeType    = null;
        this.activeAddress = null;
        this.activeBalance = 0;
        localStorage.removeItem(LS_KEY_TYPE);
        localStorage.removeItem(LS_KEY_ADDR);
        this._emit('disconnect', {});
    }

    async refreshBalance() {
        if (!this.active) return;
        try {
            this.activeBalance = await this.active.getBalance();
            this._emit('balance', { balance: this.activeBalance });
        } catch (_) {}
    }

    /** Restore a previous session on page load. */
    async restore() {
        const type    = localStorage.getItem(LS_KEY_TYPE);
        const address = localStorage.getItem(LS_KEY_ADDR);
        if (!type || !address) return;
        const adapter = this.adapters[type];
        if (!adapter) return;
        // Wait briefly for extension to inject
        await new Promise(r => setTimeout(r, 600));
        if (!adapter.isAvailable()) return;
        try {
            const connected = await adapter.isConnected();
            if (connected) {
                this.activeType    = type;
                this.activeAddress = address;
                this._emit('connect', { type, address });
                this.refreshBalance().catch(() => {});
            } else {
                localStorage.removeItem(LS_KEY_TYPE);
                localStorage.removeItem(LS_KEY_ADDR);
            }
        } catch (_) {
            localStorage.removeItem(LS_KEY_TYPE);
            localStorage.removeItem(LS_KEY_ADDR);
        }
    }

    isConnected() { return !!this.activeType; }

    /** Convenience: sign a message with the active wallet. */
    async signMessage(message) {
        if (!this.active) throw new Error('No wallet connected');
        return this.active.signMessage(message);
    }

    /** Convenience: sign a PSBT hex with the active wallet. */
    async signPSBT(psbtHex) {
        if (!this.active) throw new Error('No wallet connected');
        return this.active.signPSBT(psbtHex);
    }
}

// ---------------------------------------------------------------------------
// WalletUI — renders the connect button + modals
// ---------------------------------------------------------------------------

class WalletUI {
    constructor(manager) {
        this.manager = manager;
        manager.on((event, data) => this._onEvent(event, data));
    }

    _onEvent(event, data) {
        if (event === 'connect')    this._renderConnected(data.type, data.address);
        if (event === 'disconnect') this._renderDisconnected();
        if (event === 'balance')    this._updateBalance(data.balance);
    }

    /** Call once on DOMContentLoaded. */
    mount() {
        this._injectHTML();
        this._renderDisconnected();
        this.manager.restore();
    }

    _injectHTML() {
        // Wallet connect button injected into header
        const header = document.querySelector('header .flex.items-center.gap-3');
        if (!header) return;
        const btn = document.createElement('div');
        btn.id = 'wallet-header-slot';
        header.prepend(btn);

        // Wallet selection modal
        document.body.insertAdjacentHTML('beforeend', `
        <!-- Wallet Selection Modal -->
        <div id="wallet-modal" class="hidden fixed inset-0 bg-black/60 flex items-center justify-center z-50">
            <div class="bg-white rounded-2xl shadow-2xl w-full max-w-sm mx-4 overflow-hidden">
                <div class="bg-gradient-to-r from-orange-500 to-yellow-400 px-6 py-4">
                    <h2 class="text-white text-xl font-bold">Connect Wallet</h2>
                    <p class="text-orange-100 text-sm mt-1">Choose a Dogecoin wallet</p>
                </div>
                <div class="p-4 space-y-3" id="wallet-list"></div>
                <div class="px-4 pb-4 text-center text-xs text-gray-400">
                    Connection is stored locally and never sent to the server
                </div>
            </div>
        </div>

        <!-- Browser Wallet Import Modal -->
        <div id="browser-wallet-modal" class="hidden fixed inset-0 bg-black/60 flex items-center justify-center z-50">
            <div class="bg-white rounded-2xl shadow-2xl w-full max-w-sm mx-4 overflow-hidden">
                <div class="bg-gradient-to-r from-green-600 to-emerald-500 px-6 py-4">
                    <h2 class="text-white text-xl font-bold">🌐 Browser Wallet</h2>
                    <p class="text-green-100 text-sm mt-1">Import from WIF private key</p>
                </div>
                <div class="p-6 space-y-4">
                    <div>
                        <label class="block text-sm font-medium text-gray-700 mb-1">Dogecoin Address</label>
                        <input id="bw-address" type="text" placeholder="D…"
                               class="w-full border border-gray-300 rounded-lg px-3 py-2 text-sm font-mono focus:outline-none focus:ring-2 focus:ring-green-400">
                    </div>
                    <div>
                        <label class="block text-sm font-medium text-gray-700 mb-1">WIF Private Key</label>
                        <input id="bw-wif" type="password" placeholder="Q… or 6…"
                               class="w-full border border-gray-300 rounded-lg px-3 py-2 text-sm font-mono focus:outline-none focus:ring-2 focus:ring-green-400">
                        <p class="text-xs text-amber-600 mt-1">⚠️ Stored in your browser's localStorage only</p>
                    </div>
                    <div class="flex gap-3">
                        <button onclick="window.walletUI.closeBrowserModal()"
                                class="flex-1 border border-gray-300 text-gray-700 rounded-lg py-2 text-sm hover:bg-gray-50">
                            Cancel
                        </button>
                        <button onclick="window.walletUI.importBrowserWallet()"
                                class="flex-1 bg-green-600 text-white rounded-lg py-2 text-sm font-semibold hover:bg-green-700">
                            Import & Connect
                        </button>
                    </div>
                </div>
            </div>
        </div>
        `);

        // Close modals on backdrop click
        document.getElementById('wallet-modal').addEventListener('click', e => {
            if (e.target === document.getElementById('wallet-modal')) this.closeWalletModal();
        });
        document.getElementById('browser-wallet-modal').addEventListener('click', e => {
            if (e.target === document.getElementById('browser-wallet-modal')) this.closeBrowserModal();
        });
    }

    _walletButton({ icon, name, type, available, onClick }) {
        const unavail = !available
            ? `<span class="text-xs text-gray-400">Not installed</span>`
            : '';
        return `
        <button onclick="${onClick}" ${!available ? 'disabled' : ''}
                class="w-full flex items-center gap-3 px-4 py-3 rounded-xl border-2 ${available ? 'border-orange-200 hover:border-orange-400 hover:bg-orange-50' : 'border-gray-100 opacity-50 cursor-not-allowed'} transition-all text-left">
            <span class="text-2xl">${icon}</span>
            <div class="flex-1">
                <div class="font-semibold text-gray-800">${name}</div>
                ${unavail}
            </div>
            ${available ? '<span class="text-orange-400">→</span>' : ''}
        </button>`;
    }

    openWalletModal() {
        const list = document.getElementById('wallet-list');
        const { adapters } = this.manager;
        list.innerHTML = [
            { type: WALLET_TYPES.MYDOGE,   adapter: adapters[WALLET_TYPES.MYDOGE] },
            { type: WALLET_TYPES.DOJAK,    adapter: adapters[WALLET_TYPES.DOJAK] },
            { type: WALLET_TYPES.NINTONDO, adapter: adapters[WALLET_TYPES.NINTONDO] },
            { type: WALLET_TYPES.BROWSER,  adapter: adapters[WALLET_TYPES.BROWSER] },
        ].map(({ type, adapter }) => {
            const isBrowser = type === WALLET_TYPES.BROWSER;
            const onClick = isBrowser
                ? `window.walletUI.closeWalletModal(); window.walletUI.openBrowserModal();`
                : `window.walletUI.closeWalletModal(); window.walletUI.connectWallet('${type}');`;
            return this._walletButton({
                icon: adapter.icon,
                name: adapter.name,
                type,
                available: adapter.isAvailable(),
                onClick,
            });
        }).join('');
        document.getElementById('wallet-modal').classList.remove('hidden');
    }

    closeWalletModal() {
        document.getElementById('wallet-modal').classList.add('hidden');
    }

    openBrowserModal() {
        document.getElementById('browser-wallet-modal').classList.remove('hidden');
    }

    closeBrowserModal() {
        document.getElementById('browser-wallet-modal').classList.add('hidden');
    }

    async connectWallet(type) {
        try {
            const address = await this.manager.connect(type);
            kabosuToast(`Connected: ${address.slice(0, 8)}…${address.slice(-6)}`);
        } catch (e) {
            kabosuToast(`Connection failed: ${e.message}`, 'error');
        }
    }

    importBrowserWallet() {
        const address = document.getElementById('bw-address').value.trim();
        const wif     = document.getElementById('bw-wif').value.trim();
        if (!address.startsWith('D') || address.length < 26) {
            kabosuToast('Enter a valid Dogecoin address (starts with D)', 'error');
            return;
        }
        if (!wif) {
            kabosuToast('Enter your WIF private key', 'error');
            return;
        }
        this.manager.adapters[WALLET_TYPES.BROWSER].importWIF(wif, address);
        this.closeBrowserModal();
        this.connectWallet(WALLET_TYPES.BROWSER);
    }

    _renderConnected(type, address) {
        const adapter  = this.manager.adapters[type];
        const short    = `${address.slice(0, 8)}…${address.slice(-6)}`;
        const slot     = document.getElementById('wallet-header-slot');
        if (!slot) return;
        slot.innerHTML = `
        <div class="flex items-center gap-2 bg-white/20 backdrop-blur-sm rounded-lg px-3 py-2">
            <span class="text-lg">${adapter.icon}</span>
            <div class="text-sm">
                <div class="font-semibold font-mono">${short}</div>
                <div id="wallet-balance" class="text-xs text-orange-100">Loading…</div>
            </div>
            <button onclick="window.walletUI.manager.disconnect()"
                    class="ml-2 text-xs bg-white/20 hover:bg-white/30 rounded px-2 py-1 transition-colors">
                Disconnect
            </button>
        </div>`;
    }

    _renderDisconnected() {
        const slot = document.getElementById('wallet-header-slot');
        if (!slot) return;
        slot.innerHTML = `
        <button onclick="window.walletUI.openWalletModal()"
                class="flex items-center gap-2 bg-white/20 backdrop-blur-sm hover:bg-white/30 rounded-lg px-4 py-2 text-sm font-semibold transition-colors">
            🔗 Connect Wallet
        </button>`;
    }

    _updateBalance(balance) {
        const el = document.getElementById('wallet-balance');
        if (el) el.textContent = `${balance.toFixed(2)} DOGE`;
    }
}

// ---------------------------------------------------------------------------
// Global helper — re-uses the kabosuToast function from index.html if present
// ---------------------------------------------------------------------------

function kabosuToast(msg, type = 'success') {
    if (typeof window.showToast === 'function') {
        window.showToast(msg, type === 'error' ? 'error' : 'success');
        return;
    }
    // Minimal fallback
    const c = document.getElementById('toast-container');
    if (!c) return;
    const el = document.createElement('div');
    el.className = `px-4 py-3 rounded-lg shadow text-sm font-medium text-white ${type === 'error' ? 'bg-red-500' : 'bg-green-600'}`;
    el.textContent = msg;
    c.appendChild(el);
    setTimeout(() => el.remove(), 4000);
}

// ---------------------------------------------------------------------------
// Boot
// ---------------------------------------------------------------------------

const walletManager = new WalletManager();
const walletUI      = new WalletUI(walletManager);

window.walletManager = walletManager;
window.walletUI      = walletUI;

document.addEventListener('DOMContentLoaded', () => walletUI.mount());

