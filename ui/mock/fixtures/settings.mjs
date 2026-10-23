// The site's settings (`GET`/`PUT /settings`): its title and logo, and no support contact yet.
export const SETTINGS = { title: 'biffix (mock)', logo_data_url: null };

// Operational-alert recipients (GET/POST /settings/alerts, POST /settings/alerts/remove), by
// lower-cased email as settings.rs lists them.
export const ALERTS = {
  configured: true,
  recipients: [
    { email: 'oncall@lab.example.org', status: 'pending' },
    { email: 'ops@lab.example.org', status: 'confirmed' },
  ],
};
