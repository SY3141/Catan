(function () {
  window.hexfishGetClerkToken = async () => {
    const clerk = window.Clerk;
    if (!clerk || !clerk.session || typeof clerk.session.getToken !== 'function') {
      return null;
    }
    return clerk.session.getToken();
  };

  const loginPage = document.getElementById('login-page');
  const appShell = document.getElementById('app');
  const authMount = document.getElementById('clerk-auth-mount');
  const signInMount = document.getElementById('clerk-sign-in-mount');
  const signUpMount = document.getElementById('clerk-sign-up-mount');
  const signInTab = document.getElementById('login-tab-sign-in');
  const signUpTab = document.getElementById('login-tab-sign-up');
  const userButton = document.getElementById('clerk-user-button');
  const loginStatus = document.getElementById('clerk-login-status');
  const status = document.getElementById('clerk-status');
  let activeAuthView = 'sign-in';
  let signInMounted = false;
  let signUpMounted = false;
  let clerkNavigationGuardInstalled = false;

  if (!loginPage || !appShell || !authMount || !signInMount || !signUpMount || !signInTab || !signUpTab || !userButton) {
    return;
  }

  const setStatus = (message) => {
    if (status) status.textContent = message;
    if (loginStatus) loginStatus.textContent = message;
  };

  const emitAuthEvent = (name) => {
    document.dispatchEvent(new Event(name));
  };

  const currentPageUrl = () => (
    `${window.location.origin}${window.location.pathname}${window.location.search}`
  );

  const stripClerkRedirectParams = () => {
    const hash = window.location.hash;
    if (!hash) return null;

    const lowerHash = hash.toLowerCase();
    if (lowerHash.includes('verify-email-address')) {
      window.history.replaceState(window.history.state, '', `${currentPageUrl()}#/verify-email-address`);
      return 'sign-up';
    }
    if (lowerHash.includes('verify-phone-number')) {
      window.history.replaceState(window.history.state, '', `${currentPageUrl()}#/verify-phone-number`);
      return 'sign-up';
    }

    const [hashPath] = hash.split('?');
    const lowerHashPath = hashPath.toLowerCase();
    if (lowerHashPath === '#/sign-up') {
      window.history.replaceState(window.history.state, '', currentPageUrl());
      return 'sign-up';
    }
    if (lowerHashPath === '#/sign-in') {
      window.history.replaceState(window.history.state, '', currentPageUrl());
      return 'sign-in';
    }

    if (!hash.includes('?')) return null;

    const authHashes = [
      'continue',
      'factor-one',
      'factor-two',
      'reset-password',
      'sign-in',
      'sign-up',
      'verify-email-address',
      'verify-phone-number',
    ];
    if (!authHashes.some((path) => lowerHashPath.includes(path))) return null;

    window.history.replaceState(window.history.state, '', `${currentPageUrl()}${hashPath}`);
    return null;
  };

  const setActiveTab = (view) => {
    activeAuthView = view;
    const isSignIn = view === 'sign-in';
    signInTab.classList.toggle('active', isSignIn);
    signUpTab.classList.toggle('active', !isSignIn);
    signInTab.setAttribute('aria-selected', String(isSignIn));
    signUpTab.setAttribute('aria-selected', String(!isSignIn));
    signInMount.classList.toggle('is-hidden', !isSignIn);
    signUpMount.classList.toggle('is-hidden', isSignIn);
  };

  const syncAuthViewFromHash = () => {
    const normalizedView = stripClerkRedirectParams();
    if (normalizedView) {
      setActiveTab(normalizedView);
      return;
    }
    const hash = window.location.hash.toLowerCase();
    if (
      hash.includes('sign-up')
      || hash.includes('verify-email-address')
      || hash.includes('verify-phone-number')
      || hash.includes('continue')
    ) {
      setActiveTab('sign-up');
    } else if (
      hash.includes('sign-in')
      || hash.includes('factor-one')
      || hash.includes('factor-two')
      || hash.includes('reset-password')
    ) {
      setActiveTab('sign-in');
    }
  };

  const clearAuthHash = () => {
    if (!window.location.hash) return;
    window.history.replaceState(window.history.state, '', currentPageUrl());
  };

  const getLocalVerificationTarget = (target) => {
    if (!target) return null;
    const targetUrl = typeof target === 'string' ? target : String(target);
    const lowerTarget = targetUrl.toLowerCase();
    if (
      lowerTarget.includes('accounts.dev/sign-in')
      && lowerTarget.includes('verify-email-address')
    ) {
      return `${currentPageUrl()}#/verify-email-address`;
    }
    if (
      lowerTarget.includes('accounts.dev/sign-in')
      && lowerTarget.includes('verify-phone-number')
    ) {
      return `${currentPageUrl()}#/verify-phone-number`;
    }
    return null;
  };

  const installClerkNavigationGuard = (clerk) => {
    if (!clerk || clerkNavigationGuardInstalled) return;
    if (typeof clerk.navigate !== 'function') return;
    const originalNavigate = clerk.navigate.bind(clerk);
    clerk.navigate = async (target, ...args) => {
      const localTarget = getLocalVerificationTarget(target);
      if (localTarget) {
        setActiveTab('sign-up');
        return originalNavigate(localTarget, ...args);
      }
      return originalNavigate(target, ...args);
    };
    clerkNavigationGuardInstalled = true;
  };

  const showLoginPage = () => {
    loginPage.classList.remove('is-hidden');
    loginPage.removeAttribute('aria-hidden');
    appShell.classList.add('is-hidden');
    appShell.setAttribute('aria-hidden', 'true');
    document.body.classList.add('auth-active');
  };

  const showAppShell = () => {
    loginPage.classList.add('is-hidden');
    loginPage.setAttribute('aria-hidden', 'true');
    appShell.classList.remove('is-hidden');
    appShell.removeAttribute('aria-hidden');
    document.body.classList.remove('auth-active');
    userButton.classList.remove('hidden');
  };

  const mountCurrentAuthForm = (clerk) => {
    if (activeAuthView === 'sign-in' && !signInMounted && typeof clerk.mountSignIn === 'function') {
      clerk.mountSignIn(signInMount);
      signInMounted = true;
    }
    if (activeAuthView === 'sign-up' && !signUpMounted && typeof clerk.mountSignUp === 'function') {
      clerk.mountSignUp(signUpMount);
      signUpMounted = true;
    }
    if (activeAuthView === 'sign-up' && !signUpMounted) {
      setStatus('Sign-up is unavailable from Clerk right now.');
    }
    setActiveTab(activeAuthView);
  };

  const mountUserControl = (clerk) => {
    if (!userButton.dataset.clerkMounted) {
      clerk.mountUserButton(userButton);
      userButton.dataset.clerkMounted = 'true';
    }
  };

  const unmountUserControl = (clerk) => {
    if (!userButton.dataset.clerkMounted) return;
    if (typeof clerk.unmountUserButton === 'function') {
      clerk.unmountUserButton(userButton);
    }
    userButton.innerHTML = '';
    userButton.classList.add('hidden');
    delete userButton.dataset.clerkMounted;
  };

  const renderAuthState = () => {
    const clerk = window.Clerk;
    if (!clerk) return;
    installClerkNavigationGuard(clerk);

    if (clerk.isSignedIn || clerk.session || clerk.user) {
      showAppShell();
      mountUserControl(clerk);
      window.hexfishAuthSignedIn = true;
      emitAuthEvent('hexfish-auth-signed-in');
      setStatus('Signed in');
      return;
    }

    window.hexfishAuthSignedIn = false;
    unmountUserControl(clerk);
    showLoginPage();
    mountCurrentAuthForm(clerk);
    emitAuthEvent('hexfish-auth-signed-out');
    setStatus(activeAuthView === 'sign-up' ? 'Create your account to continue.' : 'Sign in to continue.');
  };

  signInTab.addEventListener('click', () => {
    setActiveTab('sign-in');
    clearAuthHash();
    renderAuthState();
  });

  signUpTab.addEventListener('click', () => {
    setActiveTab('sign-up');
    clearAuthHash();
    renderAuthState();
  });

  window.addEventListener('hashchange', () => {
    syncAuthViewFromHash();
    renderAuthState();
  });

  window.addEventListener('load', async () => {
    if (!window.Clerk) {
      setStatus('Clerk failed to load');
      return;
    }

    try {
      await window.Clerk.load({
        ui: { ClerkUI: window.__internal_ClerkUICtor },
      });
      installClerkNavigationGuard(window.Clerk);
      syncAuthViewFromHash();
      renderAuthState();
      if (typeof window.Clerk.addListener === 'function') {
        window.Clerk.addListener(renderAuthState);
      }
    } catch (error) {
      console.error('Clerk failed to initialize:', error);
      setStatus('Clerk failed to initialize');
    }
  });
})();
