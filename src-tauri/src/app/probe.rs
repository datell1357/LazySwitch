use serde_json::Value;
use tauri::AppHandle;

use super::windows::{open_manager, open_onboarding, open_widget, open_widget_settings};

pub fn parse_probe_report(report: &str) -> Value {
    serde_json::from_str(report).unwrap_or_else(|_| Value::String(report.to_owned()))
}

pub fn probe_script() -> String {
    r#"(() => {
      const run = async () => {
        try {
          if (!(await window.__TAURI__.core.invoke('probe:enabled'))) return;
          const page = location.pathname.split('/').pop() || '';
          const report = { page, loaded: true, rotator: typeof window.rotator, call: null };
          if (page === 'manager.html') {
            const providers = await window.rotator.providers();
            const lists = {};
            for (const provider of providers) lists[provider.id] = await window.rotator.list(provider.id);
            report.call = { providers, lists };
          } else if (page === 'onboarding.html') {
            report.call = { config: await window.rotator.getConfig(), lang: await window.rotator.getLang() };
          } else if (page === 'widget.html') {
            const config = await window.rotator.getConfig();
            const providers = await window.rotator.providers();
            const lists = {};
            for (const provider of providers) lists[provider.id] = await window.rotator.list(provider.id);
            await new Promise(resolve => setTimeout(resolve, 250));
            const windows = await window.__TAURI__.core.invoke('probe:windows');
            report.call = { config, providers, lists, windows, renderMode: document.body.classList.contains('compact') ? 'compact' : 'normal', bodyBackground: getComputedStyle(document.body).backgroundColor, rendered: document.body.innerText.length > 0 };
          } else if (page === 'widget-settings.html') {
            const config = await window.rotator.getConfig();
            const providers = await window.rotator.providers();
            const first = providers[0];
            report.call = { config, providers, firstList: first ? await window.rotator.list(first.id) : [] };
          } else return;
          await window.__TAURI__.core.invoke('probe:report', { page: page.replace('.html', ''), report: JSON.stringify(report) });
        } catch (error) {
          await window.__TAURI__.core.invoke('probe:report', { page: location.pathname, report: JSON.stringify({ loaded: true, rotator: typeof window.rotator, error: String(error) }) });
        }
      };
      setTimeout(run, 900);
    })();"#
    .to_owned()
}

pub fn open_probe_windows(app: &AppHandle) {
    open_manager(app);
    if std::env::var_os("LAZYSWITCH_PROBE_WIDGET").is_some() {
        open_widget(app, true);
        open_widget_settings(app);
    } else {
        open_onboarding(app);
        open_widget_settings(app);
    }
}
