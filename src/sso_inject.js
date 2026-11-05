// Injected by thu-learn-cli into the Tsinghua login page before page scripts run
// (CDP Page.addScriptToEvaluateOnNewDocument, see src/browser_login.rs).
//
// It mirrors learnX's src/helpers/sso.js:
// * pre-fills the username and password typed in the terminal,
// * submits the login form with the CLI's random device fingerprint, and
// * answers the two-factor "trust this device" request with that fingerprint,
// so later logins from the CLI can run over plain HTTP without two-factor prompts.
//
// What happened is recorded in localStorage on the ID origin under
// `config.storageKey`, which the CLI polls through WebDriver.
(function () {
  'use strict';

  const config = __THU_LEARN_CONFIG__;
  if (location.hostname !== config.idHost) return;

  // `是` ("yes"): the radio value that keeps this device trusted (learnX sends the same).
  const TRUST_YES = '是';
  const SAVE_FINGER_PATH = '/b/doubleAuth/personal/saveFinger';

  const remember = (patch) => {
    try {
      const previous = JSON.parse(window.localStorage.getItem(config.storageKey) || '{}');
      window.localStorage.setItem(
        config.storageKey,
        JSON.stringify(Object.assign(previous || {}, patch)),
      );
    } catch (e) {
      // Storage may be unavailable; the CLI still verifies the result over HTTP.
    }
  };

  // ---- Two-factor "trust this device" request ----

  const isSaveFinger = (url) => {
    try {
      return new URL(String(url), location.href).pathname === SAVE_FINGER_PATH;
    } catch (e) {
      return false;
    }
  };

  const rewriteTrustBody = (body) => {
    const apply = (params) => {
      params.set('fingerprint', config.fingerprint);
      params.set('deviceName', config.deviceName);
      params.set('radioVal', TRUST_YES);
      remember({ deviceTrusted: true });
    };
    if (typeof body === 'string') {
      const params = new URLSearchParams(body);
      apply(params);
      return params.toString();
    }
    if (body instanceof URLSearchParams || body instanceof FormData) {
      apply(body);
    }
    return body;
  };

  const originalOpen = XMLHttpRequest.prototype.open;
  const originalSend = XMLHttpRequest.prototype.send;
  XMLHttpRequest.prototype.open = function (method, url) {
    this.__thuLearnSaveFinger = isSaveFinger(url);
    return originalOpen.apply(this, arguments);
  };
  XMLHttpRequest.prototype.send = function (body) {
    if (this.__thuLearnSaveFinger) {
      body = rewriteTrustBody(body);
    }
    return originalSend.call(this, body);
  };

  if (typeof window.fetch === 'function') {
    const originalFetch = window.fetch;
    window.fetch = function (input, init) {
      try {
        const url = typeof input === 'string' || input instanceof URL ? input : input && input.url;
        if (isSaveFinger(url) && init && init.body !== undefined && init.body !== null) {
          init = Object.assign({}, init, { body: rewriteTrustBody(init.body) });
        }
      } catch (e) {
        // Fall through to the unmodified request.
      }
      return originalFetch.call(this, input, init);
    };
  }

  // ---- Login form submission ----

  const prepareForm = (form) => {
    if (!form || typeof form.querySelector !== 'function') return;
    const fingerPrint = form.querySelector('[name="fingerPrint"]');
    if (!fingerPrint) return; // Not the login form.
    fingerPrint.value = config.fingerprint;
    const singleLogin = form.querySelector('[name="singleLogin"]');
    if (singleLogin && !singleLogin.checked) singleLogin.click();
    const value = (name) => {
      const el = form.querySelector('[name="' + name + '"]');
      return el && el.value ? String(el.value) : '';
    };
    remember({
      submitted: true,
      fingerGenPrint: value('fingerGenPrint'),
      fingerGenPrint3: value('fingerGenPrint3'),
    });
  };

  // learnX hooks jQuery's submit(); native submit() and submit events cover other paths.
  const hookJQuery = () => {
    const $ = window.jQuery;
    if (!$ || !$.fn || typeof $.fn.submit !== 'function') return false;
    if ($.fn.submit.__thuLearnHooked) return true;
    const originalSubmit = $.fn.submit;
    $.fn.submit = function () {
      prepareForm(this[0]);
      return originalSubmit.apply(this, arguments);
    };
    $.fn.submit.__thuLearnHooked = true;
    return true;
  };
  if (!hookJQuery()) {
    const timer = setInterval(() => {
      if (hookJQuery()) clearInterval(timer);
    }, 50);
  }

  const nativeSubmit = HTMLFormElement.prototype.submit;
  HTMLFormElement.prototype.submit = function () {
    prepareForm(this);
    return nativeSubmit.apply(this, arguments);
  };
  // Capture runs before page handlers; the bubbling listener runs after them.
  document.addEventListener('submit', (event) => prepareForm(event.target), true);
  window.addEventListener('submit', (event) => prepareForm(event.target), false);

  // ---- Credential pre-fill ----

  const announced = new WeakSet();
  const setValue = (input, value) => {
    if (input.value !== value) input.value = value;
    input.readOnly = true;
    // Notify page scripts once per field; repeating could loop through the observer.
    if (!announced.has(input)) {
      announced.add(input);
      input.dispatchEvent(new Event('input', { bubbles: true }));
      input.dispatchEvent(new Event('change', { bubbles: true }));
    }
  };

  const fill = () => {
    if (typeof config.username !== 'string' || typeof config.password !== 'string') return;
    const user = document.querySelector('input#i_user');
    const pass = document.querySelector('input#i_pass');
    if (!user || !pass) return;
    setValue(user, config.username);
    setValue(pass, config.password);
  };

  new MutationObserver(fill).observe(document, { childList: true, subtree: true });
  document.addEventListener('DOMContentLoaded', fill);
  fill();
})();
