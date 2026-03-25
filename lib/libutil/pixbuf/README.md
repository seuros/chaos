# chaos-pixbuf

Loads, resizes, and re-encodes images for prompt injection. Fits
within 2048x768, preserves source format when possible, and caches
results by content hash so the same file isn't processed twice.
