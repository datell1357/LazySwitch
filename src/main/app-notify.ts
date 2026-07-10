import { app, BrowserWindow, ipcMain, screen } from "electron";
import * as path from "path";

export interface AppNotifyPayload {
  readonly title: string;
  readonly body: string;
}

interface ActiveToast {
  readonly window: BrowserWindow;
  readonly webContentsId: number;
  height: number;
}

const TOAST_WIDTH = 360;
const MIN_HEIGHT = 84;
const DEFAULT_HEIGHT = 112;
const MAX_HEIGHT = 160;
const STACK_GAP = 10;
const WORK_AREA_MARGIN = 18;
const VISIBLE_LIMIT = 4;

const payloads = new Map<number, AppNotifyPayload>();
const activeToasts: ActiveToast[] = [];
const queue: AppNotifyPayload[] = [];
let handlersRegistered = false;

function isAppQuitting(): boolean {
  return "isQuitting" in app && app.isQuitting === true;
}

function rendererPath(): string {
  return path.join(__dirname, "..", "..", "src", "renderer", "notify.html");
}

function createToastWindow(payload: AppNotifyPayload): BrowserWindow {
  return new BrowserWindow({
    width: TOAST_WIDTH,
    height: DEFAULT_HEIGHT,
    resizable: false,
    minimizable: false,
    maximizable: false,
    fullscreenable: false,
    alwaysOnTop: true,
    skipTaskbar: true,
    focusable: false,
    frame: false,
    transparent: true,
    show: false,
    backgroundColor: "#00000000",
    title: payload.title,
    webPreferences: {
      preload: preloadPath(),
      contextIsolation: true,
    },
  });
}

function preloadPath(): string {
  return path.join(__dirname, "preload.js");
}

function clampHeight(height: number): number {
  if (!Number.isFinite(height)) return DEFAULT_HEIGHT;
  return Math.min(MAX_HEIGHT, Math.max(MIN_HEIGHT, Math.ceil(height)));
}

function pruneDestroyedToasts(): void {
  for (let index = activeToasts.length - 1; index >= 0; index -= 1) {
    const toast = activeToasts[index];
    if (!toast.window.isDestroyed()) continue;
    payloads.delete(toast.webContentsId);
    activeToasts.splice(index, 1);
  }
}

function positionToasts(): void {
  pruneDestroyedToasts();
  const workArea = screen.getPrimaryDisplay().workArea;
  const x = workArea.x + workArea.width - TOAST_WIDTH - WORK_AREA_MARGIN;
  let y = workArea.y + workArea.height - WORK_AREA_MARGIN;

  for (const toast of activeToasts) {
    const win = toast.window;
    if (win.isDestroyed()) continue;
    y -= toast.height;
    try {
      win.setBounds({ x, y, width: TOAST_WIDTH, height: toast.height });
    } catch (error: unknown) {
      if (!win.isDestroyed()) throw error;
    }
    y -= STACK_GAP;
  }
}

function drainQueue(): void {
  pruneDestroyedToasts();
  if (isAppQuitting()) {
    queue.length = 0;
    return;
  }
  while (activeToasts.length < VISIBLE_LIMIT) {
    const payload = queue.shift();
    if (!payload) return;
    showNow(payload);
  }
}

function registerHandlers(): void {
  if (handlersRegistered) return;
  handlersRegistered = true;

  ipcMain.handle(
    "app-notify:payload",
    (event) => payloads.get(event.sender.id) ?? null
  );

  ipcMain.on("app-notify:resize", (event, height: unknown) => {
    if (typeof height !== "number") return;
    const toast = activeToasts.find((item) => item.webContentsId === event.sender.id);
    if (!toast) return;

    const nextHeight = clampHeight(height);
    if (toast.height === nextHeight) return;
    toast.height = nextHeight;
    positionToasts();
  });

  ipcMain.on("app-notify:dismiss", (event) => {
    const win = BrowserWindow.fromWebContents(event.sender);
    if (win && !win.isDestroyed()) win.close();
  });
}

function removeToast(webContentsId: number): void {
  payloads.delete(webContentsId);
  const index = activeToasts.findIndex((toast) => toast.webContentsId === webContentsId);
  if (index >= 0) activeToasts.splice(index, 1);
  positionToasts();
  drainQueue();
}

function showNow(payload: AppNotifyPayload): void {
  let win: BrowserWindow;
  try {
    win = createToastWindow(payload);
  } catch (error: unknown) {
    // Toasts are best-effort: a failed window must never bubble into the
    // switch/notify caller and turn a successful switch into an error.
    const message = error instanceof Error ? error.message : String(error);
    console.warn(`Failed to create in-app notification window: ${message}`);
    return;
  }
  const toast: ActiveToast = {
    window: win,
    webContentsId: win.webContents.id,
    height: DEFAULT_HEIGHT,
  };
  activeToasts.push(toast);
  payloads.set(toast.webContentsId, payload);
  positionToasts();

  win.on("closed", () => removeToast(toast.webContentsId));
  void win
    .loadFile(rendererPath())
    .then(() => {
      if (!win.isDestroyed()) {
        positionToasts();
        win.showInactive();
      }
    })
    .catch((error: unknown) => {
      const message = error instanceof Error ? error.message : String(error);
      console.warn(`Failed to show in-app notification: ${message}`);
      if (!win.isDestroyed()) win.close();
    });
}

export function showAppNotification(payload: AppNotifyPayload): void {
  registerHandlers();
  if (!app.isReady()) {
    queue.push(payload);
    void app.whenReady().then(drainQueue);
    return;
  }
  if (activeToasts.length >= VISIBLE_LIMIT) {
    queue.push(payload);
    return;
  }
  showNow(payload);
}
