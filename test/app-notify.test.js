const assert = require("node:assert/strict");
const { EventEmitter } = require("node:events");
const test = require("node:test");

function loadAppNotify(options = {}) {
  const electronPath = require.resolve("electron");
  const modulePath = require.resolve("../dist/main/app-notify.js");
  delete require.cache[modulePath];

  const windows = [];
  let ready = options.ready ?? true;
  let resolveReady;
  const readyPromise = new Promise((resolve) => {
    resolveReady = resolve;
  });

  class FakeBrowserWindow extends EventEmitter {
    constructor() {
      super();
      if (options.throwOnCreate) {
        throw new Error("window creation failed");
      }
      this.destroyed = false;
      this.webContentsValue = { id: windows.length + 1 };
      this.bounds = [];
      this.shown = false;
      windows.push(this);
    }

    get webContents() {
      if (this.destroyed) {
        throw new TypeError("Object has been destroyed");
      }
      return this.webContentsValue;
    }

    isDestroyed() {
      return this.destroyed;
    }

    setBounds(bounds) {
      if (this.destroyed) {
        throw new TypeError("Object has been destroyed");
      }
      this.bounds.push(bounds);
    }

    showInactive() {
      if (this.destroyed) {
        throw new TypeError("Object has been destroyed");
      }
      this.shown = true;
    }

    loadFile() {
      return Promise.resolve();
    }

    close() {
      if (this.destroyed) return;
      this.destroyed = true;
      this.emit("closed");
    }

    static fromWebContents(sender) {
      return windows.find((win) => !win.destroyed && win.webContentsValue === sender) ?? null;
    }
  }

  require.cache[electronPath] = {
    id: electronPath,
    filename: electronPath,
    loaded: true,
    exports: {
      app: {
        isQuitting: false,
        isReady: () => ready,
        whenReady: () => readyPromise,
      },
      BrowserWindow: FakeBrowserWindow,
      ipcMain: {
        handle: () => undefined,
        on: () => undefined,
      },
      screen: {
        getPrimaryDisplay: () => ({
          workArea: { x: 0, y: 0, width: 1200, height: 900 },
        }),
      },
    },
  };

  return {
    electron: require.cache[electronPath].exports,
    module: require(modulePath),
    resolveReady,
    setReady: (value) => {
      ready = value;
    },
    windows,
  };
}

test("showAppNotification compacts destroyed toast before draining queue", () => {
  const { module, windows } = loadAppNotify();

  for (let index = 0; index < 5; index += 1) {
    module.showAppNotification({ title: `Title ${index}`, body: "Body" });
  }

  assert.equal(windows.length, 4);
  assert.doesNotThrow(() => windows[0].close());
  assert.equal(windows.length, 5);
});

test("showAppNotification drops queued toast when app quit begins before ready", async () => {
  const { electron, module, resolveReady, setReady, windows } = loadAppNotify({
    ready: false,
  });

  module.showAppNotification({ title: "Title", body: "Body" });
  electron.app.isQuitting = true;
  setReady(true);
  resolveReady();
  await Promise.resolve();

  assert.equal(windows.length, 0);
});

test("showAppNotification ignores BrowserWindow creation failure", () => {
  const { module, windows } = loadAppNotify({ throwOnCreate: true });

  assert.doesNotThrow(() =>
    module.showAppNotification({ title: "Title", body: "Body" })
  );
  assert.equal(windows.length, 0);
});
