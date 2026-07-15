(() => {
  const tauri = window.__TAURI__;
  const core = tauri && tauri.core;
  const event = tauri && tauri.event;
  if (!core || !event) return;

  const invoke = (command, args) => core.invoke(command, args);
  const send = (command, args) => {
    void invoke(command, args).catch(() => undefined);
  };

  window.rotator = {
    respond: (approved) => send("approval:respond", { approved }),
    cliRestartPayload: () => invoke("cli-restart:payload"),
    cliRestartRespond: (action) => send("cli-restart:respond", { action }),
    appNotifyPayload: () => invoke("app-notify:payload"),
    appNotifyResize: (height) => send("app-notify:resize", { height }),
    appNotifyDismiss: () => send("app-notify:dismiss"),

    providers: () => invoke("providers:list"),
    list: (provider) => invoke("accounts:list", { provider }),
    usageHistory: (provider, name) => invoke("usage:history", { provider, name }),
    switchTo: (provider, name) => invoke("accounts:switch", { provider, name }),
    setEnabled: (provider, name, enabled) =>
      invoke("accounts:setEnabled", { provider, name, enabled }),
    remove: (provider, name) => invoke("accounts:remove", { provider, name }),
    rename: (provider, oldName, newName) =>
      invoke("accounts:rename", { provider, oldName, newName }),
    importCurrent: (provider, name) =>
      invoke("accounts:importCurrent", { provider, name }),
    addViaLogin: (provider) => invoke("accounts:addViaLogin", { provider }),
    testCliRestart: (provider) => invoke("cli:testRestart", { provider }),
    getConfig: () => invoke("config:get"),
    getLang: () => invoke("lang:get"),
    setConfig: (patch) => invoke("config:set", { patch }),
    widgetCompactHeight: (height) => send("widget:compact-height", { height }),
    widgetContextMenu: () => send("widget:context-menu"),

    finishOnboarding: (openAccounts) =>
      invoke("onboarding:finish", { openAccounts }),

    onChanged: (cb) => event.listen("accounts:changed", () => cb()),
    onWidgetTaskbarTheme: (cb) =>
      event.listen("widget:taskbar-theme", (e) => cb(e.payload ?? null)),
    onLoginUrl: (cb) => event.listen("login:url", (e) => cb(e.payload)),
    openUrl: (url) => invoke("open:url", { url }),
    closeManager: () => invoke("manager:close"),
    closeWidget: () => invoke("widget:close"),
    closeWidgetSettings: () => invoke("widget-settings:close"),
  };

  // Electron's CSS app-region is not understood by Tauri. Keep the renderer
  // markup intact and start a native drag from the existing titlebar instead.
  document.addEventListener("mousedown", (event) => {
    if (event.button !== 0) return;
    const target = event.target instanceof Element ? event.target : null;
    const drag = target && target.closest(".titlebar, [style*='app-region: drag']");
    if (!drag || target.closest("button, input, select, textarea, a, [style*='no-drag']")) return;
    send("window:startDragging");
  }, true);

  // The compact widget has no titlebar, so its menu lives on right-click.
  // Electron hooked WM_CONTEXTMENU for this; here the webview sees the event.
  document.addEventListener("contextmenu", (event) => {
    if (!document.body.classList.contains("compact")) return;
    event.preventDefault();
    send("widget:context-menu");
  });
})();
