(function () {
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

    if (clerk.isSignedIn || clerk.user) {
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
    renderAuthState();
  });

  signUpTab.addEventListener('click', () => {
    setActiveTab('sign-up');
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
