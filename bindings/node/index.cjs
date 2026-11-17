const path = require('node:path');

const target = `${process.platform}-${process.arch}`;
if (!['darwin-arm64', 'win32-x64'].includes(target)) {
  throw new Error(`Goku supports macOS arm64 and Windows x64. This process is ${target}.`);
}

module.exports = require(path.join(__dirname, 'native', `${target}.node`));
