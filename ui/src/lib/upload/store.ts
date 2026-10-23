// The app's one set of uploads: the tray shows it, file fields show their file's progress, and the
// launch form waits for files still uploading. Lives for the tab, across page navigation.

import { apiTransport } from './transport';
import { Uploads } from './uploads.svelte';

export const uploads = new Uploads(apiTransport);
