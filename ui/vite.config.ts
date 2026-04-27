import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';

export default defineConfig({
  plugins: [react()],
  base: '/ui/',
  build: {
    outDir: 'dist',
    emptyOutDir: true,
  },
  server: {
    // For `npm run dev`, proxy FHIR API calls to a locally-running sazare-server.
    // The UI itself is served by Vite at /ui/; everything else goes to the server.
    proxy: {
      '^/(ServiceRequest|Patient|Observation|Encounter|Bundle|metadata|health|\\$)': {
        target: 'http://localhost:8080',
        changeOrigin: true,
      },
    },
  },
});
