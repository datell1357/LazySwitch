(() => {
  const { invoke } = window.__TAURI__.core;
  const { listen } = window.__TAURI__.event;

  if (window.location.pathname.endsWith("/widget.html")) {
    document.addEventListener("DOMContentLoaded", () => {
      document.querySelector(".titlebar")?.setAttribute("data-tauri-drag-region", "deep");
    });
  }

  window.rotator = {
    respond: (approved) => invoke("approval_respond", { approved }),
    cliRestartPayload: () => invoke("cli_restart_payload"),
    cliRestartRespond: (action) => invoke("cli_restart_respond", { action }),
    appNotifyPayload: () => invoke("app_notify_payload"),
    appNotifyResize: (height) => invoke("app_notify_resize", { height }),
    appNotifyDismiss: () => invoke("app_notify_dismiss"),

    providers: () => invoke("providers_list"),
    list: (provider) => invoke("accounts_list", { pid: provider }),
    switchTo: (provider, name) =>
      invoke("accounts_switch", { pid: provider, name }),
    setEnabled: (provider, name, enabled) =>
      invoke("accounts_set_enabled", { pid: provider, name, enabled }),
    remove: (provider, name) =>
      invoke("accounts_remove", { pid: provider, name }),
    rename: (provider, oldName, newName) =>
      invoke("accounts_rename", { pid: provider, oldName, newName }),
    importCurrent: (provider, name) =>
      invoke("accounts_import_current", { pid: provider, name }),
    addViaLogin: (provider) =>
      invoke("accounts_add_via_login", { pid: provider }),
    testCliRestart: (provider) =>
      invoke("cli_test_restart", { pid: provider }),
    getConfig: () => invoke("config_get"),
    getLang: () => invoke("lang_get"),
    setConfig: (patch) => invoke("config_set", { patch }),
    widgetCompactHeight: (height) =>
      invoke("widget_compact_height", { height }),

    finishOnboarding: (openAccounts) =>
      invoke("onboarding_finish", { openAccounts }),

    onChanged: (cb) => listen("accounts:changed", () => cb()),
    onWidgetTaskbarTheme: (cb) =>
      listen("widget:taskbar-theme", (event) => cb(event.payload)),
    onLoginUrl: (cb) =>
      listen("login:url", (event) => cb(event.payload)),
    openUrl: (url) => invoke("open_url", { url }),
    closeManager: () => invoke("manager_close"),
    closeWidget: () => invoke("widget_close"),
    closeWidgetSettings: () => invoke("widget_settings_close"),
  };
})();
