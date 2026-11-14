'use strict';
// [ADR 0085] The reporter hushrun's Jest adapter adds to Jest's reporters, in the process that
// claimed the run (Jest's public Reporter API): when the run starts (the globalSetup window ends),
// how each test file ended as Jest's main process saw it, and the main process's file when the run
// ends. It decides nothing, and never throws into Jest.
const S = globalThis[Symbol.for('hushrun.jest')];

class HushrunReporter {
  constructor(globalConfig) {
    this.globalConfig = globalConfig;
  }

  onRunStart() {
    if (!S || !S.main) return;
    try {
      S.runStarted();
    } catch (err) {
      S.note('reporter', err);
    }
  }

  onTestFileResult(test, result) {
    if (!S || !S.main) return;
    try {
      S.fileResult(test, result, this.globalConfig);
    } catch (err) {
      S.note('reporter', err);
    }
  }

  onRunComplete(contexts, results) {
    if (!S || !S.main) return;
    try {
      S.writeMain(results);
    } catch (err) {
      S.note('reporter', err);
    }
  }
}
module.exports = HushrunReporter;
