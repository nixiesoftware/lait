/// <reference types="vite/client" />

interface ImportMetaEnv {
  readonly VITE_URL_BASE: string;
  readonly VITE_API_BASE: string;
  readonly VITE_API_URL: string;
  readonly VITE_CLOUD_STORAGE_URL: string;
}

interface ImportMeta {
  readonly env: ImportMetaEnv;
}
