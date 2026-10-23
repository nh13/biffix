// The answers a read of one object gives, the same for a run's output (outputs.rs) and a data
// source's file (data.rs): its preview and its download link.

/** A preview of an object: its `text`, or a binary one when it has none. */
export const previewBody = (text) =>
  text == null
    ? { kind: 'binary', text: '', truncated: false, bytes_read: 1024, decompressed: false }
    : {
        kind: 'text',
        text,
        truncated: false,
        bytes_read: text.length,
        total_size: text.length,
        decompressed: false,
      };

/** A download link to an object named `name` (links last ten minutes). */
export const downloadLink = (name) => ({
  url: `data:text/plain,mock%20download%20of%20${encodeURIComponent(name)}`,
  expires_at: Date.now() / 1000 + 600,
  filename: name,
});
