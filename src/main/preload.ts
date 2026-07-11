import { contextBridge, ipcRenderer } from "electron";

contextBridge.exposeInMainWorld("rotator", {
  // Approval popup
  respond: (approved: boolean) => ipcRenderer.send("approval:respond", approved),
  cliRestartPayload: () => ipcRenderer.invoke("cli-restart:payload"),
  cliRestartRespond: (action: string) =>
    ipcRenderer.send("cli-restart:respond", action),
  appNotifyPayload: () => ipcRenderer.invoke("app-notify:payload"),
  appNotifyResize: (height: number) =>
    ipcRenderer.send("app-notify:resize", height),
  appNotifyDismiss: () => ipcRenderer.send("app-notify:dismiss"),

  // Account manager window (provider-scoped)
  providers: () => ipcRenderer.invoke("providers:list"),
  list: (provider: string) => ipcRenderer.invoke("accounts:list", provider),
  switchTo: (provider: string, name: string) =>
    ipcRenderer.invoke("accounts:switch", provider, name),
  setEnabled: (provider: string, name: string, enabled: boolean) =>
    ipcRenderer.invoke("accounts:setEnabled", provider, name, enabled),
  remove: (provider: string, name: string) =>
    ipcRenderer.invoke("accounts:remove", provider, name),
  rename: (provider: string, oldName: string, newName: string) =>
    ipcRenderer.invoke("accounts:rename", provider, oldName, newName),
  importCurrent: (provider: string, name?: string) =>
    ipcRenderer.invoke("accounts:importCurrent", provider, name),
  addViaLogin: (provider: string) =>
    ipcRenderer.invoke("accounts:addViaLogin", provider),
  testCliRestart: (provider: string) =>
    ipcRenderer.invoke("cli:testRestart", provider),
  getConfig: () => ipcRenderer.invoke("config:get"),
  getLang: () => ipcRenderer.invoke("lang:get"),
  setConfig: (patch: unknown) => ipcRenderer.invoke("config:set", patch),

  // Push updates from main → renderer (e.g. after an auto-switch)
  onChanged: (cb: () => void) => ipcRenderer.on("accounts:changed", () => cb()),
  onLoginUrl: (cb: (url: string) => void) =>
    ipcRenderer.on("login:url", (_e, url: string) => cb(url)),
  openUrl: (url: string) => ipcRenderer.invoke("open:url", url),
  closeManager: () => ipcRenderer.invoke("manager:close"),
});
