(function () {
  const loginPage = document.getElementById('login-page');
  const appShell = document.getElementById('app');
  const authMount = document.getElementById('clerk-auth-mount');
  const signInMount = document.getElementById('clerk-sign-in-mount');
  const signUpMount = document.getElementById('clerk-sign-up-mount');
  const signInTab = document.getElementById('login-tab-sign-in');
  const signUpTab = document.getElementById('login-tab-sign-up');
  const loginPanelTitle = document.getElementById('login-panel-title');
  const loginPanelCopy = document.getElementById('login-panel-copy');
  const continueGuestButton = document.getElementById('btn-continue-guest');
  const guestSignUpButton = document.getElementById('btn-guest-sign-up');
  const userButton = document.getElementById('clerk-user-button');
  const loginStatus = document.getElementById('clerk-login-status');
  const status = document.getElementById('clerk-status');
  let activeAuthView = 'sign-in';
  let signInMounted = false;
  let signUpMounted = false;
  let clerkNavigationGuardInstalled = false;
  let suppressInviteGuestMode = false;

  if (!loginPage || !appShell || !authMount || !signInMount || !signUpMount || !signInTab || !signUpTab || !userButton) {
    return;
  }

  const authCopy = {
    'sign-in': {
      title: 'Welcome back',
      panel: 'Sign in to access your saved replays, rooms, and analysis tools.',
    },
    'sign-up': {
      title: 'Create your account',
      panel: 'Create a free account to save replay history, copy share links, and continue games across devices.',
    },
  };

  const normalizeUsername = (value) => (
    String(value || '')
      .replace(/[\u0000-\u001f\u007f]/g, '')
      .replace(/\s+/g, ' ')
      .trim()
      .slice(0, 24)
  );

  const clerkUsername = () => (
    normalizeUsername(window.Clerk?.user?.username)
  );

  const clerkUserDisplayNameFallback = () => {
    const user = window.Clerk?.user;
    if (!user) return '';
    const firstLast = [user.firstName, user.lastName].filter(Boolean).join(' ');
    const emailPrefix = user.primaryEmailAddress?.emailAddress?.split('@')[0] || '';
    const candidates = [firstLast, user.fullName, emailPrefix];
    return normalizeUsername(candidates.find((candidate) => normalizeUsername(candidate)) || '');
  };

  const currentAuthUsername = () => (
    normalizeUsername(
      clerkUsername()
      || clerkUserDisplayNameFallback()
    )
  );

  window.hexfishUsername = currentAuthUsername();
  window.hexfishAuthUsername = () => (
    window.hexfishAuthSignedIn ? currentAuthUsername() : ''
  );

  const syncAuthMountHeight = () => {
    const update = () => {
      const current = parseFloat(authMount.style.minHeight) || 0;
      const next = Math.max(
        430,
        current,
        signInMount.scrollHeight || 0,
        signUpMount.scrollHeight || 0
      );
      authMount.style.minHeight = `${Math.ceil(next)}px`;
    };
    requestAnimationFrame(update);
    window.setTimeout(update, 250);
    window.setTimeout(update, 1000);
  };

  const setStatus = (message) => {
    if (status) status.textContent = message;
    if (loginStatus) {
      loginStatus.textContent = message || '\u00a0';
      loginStatus.classList.toggle('login-status-empty', !message);
    }
  };

  const emitAuthEvent = (name) => {
    document.dispatchEvent(new Event(name));
  };

  const currentPageUrl = () => (
    `${window.location.origin}${window.location.pathname}${window.location.search}`
  );

  const normalizeRoomCode = (code) => (
    String(code || '').trim().toUpperCase().replace(/[^A-Z0-9]/g, '').slice(0, 12)
  );

  const currentRoomCode = () => (
    normalizeRoomCode(new URLSearchParams(window.location.search).get('room'))
  );

  const normalizeReplaySlug = (slug) => (
    String(slug || '').trim().replace(/[^A-Za-z0-9_-]/g, '').slice(0, 128)
  );

  const currentReplaySlug = () => (
    normalizeReplaySlug(new URLSearchParams(window.location.search).get('replay'))
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

    const copy = authCopy[view] || authCopy['sign-in'];
    if (loginPanelTitle) loginPanelTitle.textContent = copy.title;
    if (loginPanelCopy) loginPanelCopy.textContent = copy.panel;
    syncAuthMountHeight();
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

  const showLocalAuthTab = (view) => {
    suppressInviteGuestMode = true;
    setActiveTab(view);
    clearAuthHash();
    renderAuthState();
  };

  const authViewForTarget = (target) => {
    if (!target) return null;
    const value = typeof target === 'string' ? target : String(target);
    const lowerValue = value.toLowerCase();
    if (
      lowerValue.includes('verify-email-address')
      || lowerValue.includes('verify-phone-number')
      || lowerValue.includes('factor-one')
      || lowerValue.includes('factor-two')
      || lowerValue.includes('continue')
      || lowerValue.includes('reset-password')
    ) {
      return null;
    }
    if (lowerValue.includes('sign-up')) return 'sign-up';
    if (lowerValue.includes('sign-in')) return 'sign-in';
    return null;
  };

  const maybeUseLocalAuthNavigation = (target) => {
    const view = authViewForTarget(target);
    if (!view) return false;
    showLocalAuthTab(view);
    return true;
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
      if (maybeUseLocalAuthNavigation(target)) return;
      return originalNavigate(target, ...args);
    };
    clerkNavigationGuardInstalled = true;
  };

  const handleAuthMountClick = (event) => {
    const link = event.target?.closest?.('a[href]');
    if (!link || !authMount.contains(link)) return;
    if (!maybeUseLocalAuthNavigation(link.href)) return;
    event.preventDefault();
    event.stopPropagation();
  };

  const showLoginPage = () => {
    loginPage.classList.remove('is-hidden');
    loginPage.removeAttribute('aria-hidden');
    appShell.classList.add('is-hidden');
    appShell.setAttribute('aria-hidden', 'true');
    document.body.classList.add('auth-active');
    guestSignUpButton?.classList.add('hidden');
  };

  const showAppShell = (options = {}) => {
    const showUserControl = options.showUserControl !== false;
    loginPage.classList.add('is-hidden');
    loginPage.setAttribute('aria-hidden', 'true');
    appShell.classList.remove('is-hidden');
    appShell.removeAttribute('aria-hidden');
    document.body.classList.remove('auth-active');
    userButton.classList.toggle('hidden', !showUserControl);
  };

  const setGuestHeaderSignUpVisible = (visible) => {
    guestSignUpButton?.classList.toggle('hidden', !visible);
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
    syncAuthMountHeight();
  };

  const mountUserControl = (clerk) => {
    if (!userButton.dataset.clerkMounted) {
      clerk.mountUserButton(userButton);
      userButton.dataset.clerkMounted = 'true';
    }
  };

  const unmountUserControl = (clerk) => {
    if (!userButton.dataset.clerkMounted) return;
    if (clerk && typeof clerk.unmountUserButton === 'function') {
      clerk.unmountUserButton(userButton);
    }
    userButton.innerHTML = '';
    userButton.classList.add('hidden');
    delete userButton.dataset.clerkMounted;
  };

  const clearGuestState = () => {
    window.hexfishGuestMultiplayer = false;
    window.hexfishGuestRoomCode = '';
    window.hexfishGuestSharedReplay = false;
    window.hexfishGuestReplaySlug = '';
  };

  const enterGuestMode = () => {
    const clerk = window.Clerk;
    suppressInviteGuestMode = false;
    window.hexfishAuthSignedIn = false;
    window.hexfishGuestMultiplayer = true;
    window.hexfishGuestRoomCode = currentRoomCode();
    window.hexfishGuestSharedReplay = false;
    window.hexfishGuestReplaySlug = '';
    unmountUserControl(clerk);
    showAppShell({ showUserControl: false });
    setGuestHeaderSignUpVisible(true);
    setStatus('Guest multiplayer');
    emitAuthEvent('hexfish-auth-guest');
  };

  const enterGuestSharedReplayMode = () => {
    const clerk = window.Clerk;
    const slug = currentReplaySlug();
    if (!slug) return false;
    suppressInviteGuestMode = false;
    window.hexfishAuthSignedIn = false;
    window.hexfishGuestMultiplayer = false;
    window.hexfishGuestRoomCode = '';
    window.hexfishGuestSharedReplay = true;
    window.hexfishGuestReplaySlug = slug;
    unmountUserControl(clerk);
    showAppShell({ showUserControl: false });
    setGuestHeaderSignUpVisible(true);
    setStatus('Viewing shared replay');
    emitAuthEvent('hexfish-auth-guest');
    return true;
  };

  const showAuthPageFromGuest = (view) => {
    const clerk = window.Clerk;
    const nextView = view === 'sign-up' ? 'sign-up' : 'sign-in';
    suppressInviteGuestMode = true;
    window.hexfishAuthSignedIn = false;
    clearGuestState();
    setGuestHeaderSignUpVisible(false);
    emitAuthEvent('hexfish-auth-signed-out');
    setActiveTab(nextView);
    showLoginPage();
    if (!clerk) {
      setStatus(nextView === 'sign-up' ? 'Sign-up is unavailable right now.' : 'Sign-in is unavailable right now.');
      return;
    }
    installClerkNavigationGuard(clerk);
    mountCurrentAuthForm(clerk);
    if ((nextView === 'sign-up' && signUpMounted) || (nextView === 'sign-in' && signInMounted)) {
      setStatus('');
    }
  };

  const showSignUpPageFromGuest = () => showAuthPageFromGuest('sign-up');
  const showSignInPageFromGuest = () => showAuthPageFromGuest('sign-in');

  window.hexfishShowSignInFromGuest = showSignInPageFromGuest;
  window.hexfishShowSignUpFromGuest = showSignUpPageFromGuest;

  const renderAuthState = () => {
    const clerk = window.Clerk;
    if (!clerk) return;
    installClerkNavigationGuard(clerk);

    if (clerk.isSignedIn || clerk.session || clerk.user) {
      showAppShell();
      mountUserControl(clerk);
      suppressInviteGuestMode = false;
      window.hexfishAuthSignedIn = true;
      window.hexfishUsername = currentAuthUsername();
      clearGuestState();
      setGuestHeaderSignUpVisible(false);
      emitAuthEvent('hexfish-auth-signed-in');
      setStatus('Signed in');
      return;
    }

    if (window.hexfishGuestSharedReplay || (!suppressInviteGuestMode && currentReplaySlug())) {
      enterGuestSharedReplayMode();
      return;
    }

    if (window.hexfishGuestMultiplayer || (!suppressInviteGuestMode && currentRoomCode())) {
      enterGuestMode();
      return;
    }

    window.hexfishAuthSignedIn = false;
    clearGuestState();
    unmountUserControl(clerk);
    showLoginPage();
    mountCurrentAuthForm(clerk);
    emitAuthEvent('hexfish-auth-signed-out');
    if (activeAuthView !== 'sign-up' || signUpMounted) setStatus('');
  };

  signInTab.addEventListener('click', () => {
    showLocalAuthTab('sign-in');
  });

  signUpTab.addEventListener('click', () => {
    showLocalAuthTab('sign-up');
  });

  continueGuestButton?.addEventListener('click', () => {
    clearAuthHash();
    enterGuestMode();
  });

  guestSignUpButton?.addEventListener('click', showSignUpPageFromGuest);
  authMount.addEventListener('click', handleAuthMountClick, true);

  window.addEventListener('hashchange', () => {
    syncAuthViewFromHash();
    renderAuthState();
  });

  window.addEventListener('load', async () => {
    if (!window.Clerk) {
      if (currentReplaySlug()) {
        enterGuestSharedReplayMode();
        return;
      }
      if (currentRoomCode()) {
        enterGuestMode();
        return;
      }
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
