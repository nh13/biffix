// What the Settings page loads, one class per thing: the site settings (shared by the Site and
// Support contact forms and the Lab view's "Who to ask"), the engine releases, and the alert
// recipients. The page creates them, so each loads once however the sections are shown.

import {
  addAlertRecipient,
  getAlertRecipients,
  getSettings,
  listEngines,
  removeAlertRecipient,
  type AlertRecipientList,
  type EngineSupport,
  type SiteSettings,
} from '$lib/api';
import { setSite } from '$lib/state/site.svelte';
import { errorMessage } from '$lib/util/format';

/**
 * What the server has stored, as last loaded or saved. The forms are offered only once this has
 * loaded, and each saves only the fields that differ from it.
 */
export class SiteSettingsState {
  stored = $state<SiteSettings | null>(null);
  loading = $state(true);
  loadError = $state<string | null>(null);

  async load(): Promise<void> {
    this.loading = true;
    this.loadError = null;
    try {
      this.saved(await getSettings());
    } catch (e) {
      this.loadError = errorMessage(e);
    } finally {
      this.loading = false;
    }
  }

  /**
   * A load or save came back: it is what is stored now, and the chrome (sidebar, top bar, tab
   * title, favicon) shows it at once.
   */
  saved(s: SiteSettings): void {
    this.stored = s;
    setSite(s);
  }
}

/** The engine releases a workflow version may pin (read-only; set by the biffix release). */
export class EnginesState {
  engines = $state<EngineSupport[] | null>(null);
  error = $state<string | null>(null);

  load(): void {
    listEngines()
      .then((e) => (this.engines = e))
      .catch((e) => (this.error = errorMessage(e)));
  }
}

/** Operational-alert recipients: the alerts topic's email subscribers (developer only). */
export class AlertsState {
  alerts = $state<AlertRecipientList | null>(null);
  error = $state<string | null>(null);
  busy = $state(false);

  load(): void {
    getAlertRecipients()
      .then((a) => (this.alerts = a))
      .catch((e) => (this.error = errorMessage(e)));
  }

  /** Adds `email`; whether it was added (the field is cleared then). */
  async add(email: string): Promise<boolean> {
    this.busy = true;
    this.error = null;
    try {
      this.alerts = await addAlertRecipient(email);
      return true;
    } catch (e) {
      this.error = errorMessage(e);
      return false;
    } finally {
      this.busy = false;
    }
  }

  async remove(email: string): Promise<void> {
    this.busy = true;
    this.error = null;
    try {
      await removeAlertRecipient(email);
      this.alerts = await getAlertRecipients();
    } catch (e) {
      this.error = errorMessage(e);
    } finally {
      this.busy = false;
    }
  }
}
