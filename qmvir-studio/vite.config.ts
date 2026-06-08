import { defineConfig } from 'vite'
import solid from 'vite-plugin-solid'

export default defineConfig({
  plugins: [solid()],
  // Prevent Vite from watching Cargo build artifacts
  server: {
    watch: {
      ignored: ['**/src-tauri/**'],
    },
  },
})
