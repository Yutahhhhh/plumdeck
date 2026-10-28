// Extract the pinned RubberBand source after checking the archive hash.
// Usage: node prepare-rubberband.mjs <dependency root containing source.tar.bz2>
import { resolve } from 'node:path';
import { extractPinnedSource } from './extract-pinned-source.mjs';

if (!process.argv[2]) throw new Error('Usage: node prepare-rubberband.mjs <dependency root>');
extractPinnedSource({
    root: resolve(process.argv[2]),
    archive: 'source.tar.bz2',
    prefix: 'rubberband-4.0.0',
    sha256: 'af050313ee63bc18b35b2e064e5dce05b276aaf6d1aa2b8a82ced1fe2f8028e9',
});
