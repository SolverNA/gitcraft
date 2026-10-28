const {defineConfig} = require('@playwright/test');
module.exports = defineConfig({
  testDir: './tests/browser',
  timeout: 60000,
  workers: 1,
  // Keep scoped tokens and write-only credentials out of retained browser traces.
  use: {browserName: 'chromium', headless: true, trace: 'off'},
  reporter: 'list',
  outputDir: 'test-results/browser'
});
