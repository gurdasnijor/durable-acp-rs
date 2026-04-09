import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

export default defineConfig({
  plugins: [react()],
  server: {
    port: 5173,
    proxy: {
      "/acp": {
        target: "http://localhost:4438",
        ws: true,
      },
      "/api": {
        target: "http://localhost:4438",
      },
      "/streams": {
        target: "http://localhost:4437",
      },
    },
  },
});
