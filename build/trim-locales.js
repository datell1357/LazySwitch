// Electron ships 55 locale packs (~41 MB). LazySwitch only speaks these four,
// so drop the rest: a smaller payload is a faster install.
const fs = require("fs");
const path = require("path");

const KEEP = new Set(["en-US.pak", "ko.pak", "ja.pak", "zh-CN.pak", "zh-TW.pak"]);

exports.default = async function trimLocales(context) {
  const dir = path.join(context.appOutDir, "locales");
  if (!fs.existsSync(dir)) return;
  let removed = 0;
  let freed = 0;
  for (const file of fs.readdirSync(dir)) {
    if (KEEP.has(file)) continue;
    const target = path.join(dir, file);
    freed += fs.statSync(target).size;
    fs.rmSync(target);
    removed += 1;
  }
  console.log(`  • trimmed locales  removed=${removed} freed=${(freed / 1048576).toFixed(1)}MB`);
};
