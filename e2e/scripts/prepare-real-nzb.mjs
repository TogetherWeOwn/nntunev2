#!/usr/bin/env node
/**
 * Build the staging-only real-NZB fixture from the secret-held full NZB.
 *
 * The full secret NZB references thousands of segments (~1.57 GB); ingesting
 * it whole on every push cannot pass the journey's ingest-status timeout and
 * would pull the full release through the provider each run. This script
 * writes a single-file subset — the first non-PAR2 `<file>` block, verbatim,
 * with all of its segments — so the journey ingests one whole data file
 * (whole-file: a partial file would trip the verify/repair path) and proves
 * provider reachability, intake acceptance, segment presence (no 430) and
 * matching, without the full download.
 *
 * Owner rule (public repo): no NZB bytes, message-IDs, subjects, file names
 * or segment lists in the repo or public logs. This script upholds it:
 *   - paths travel as argv only (`--input`, `--output`); content is never
 *     printed — stdout carries only the input/output FILE counts (bare
 *     integers, never IDs, subjects, names or segment lists);
 *   - failures name the check that failed (byte cap, no file blocks, empty
 *     subset, write error) with counts only, never content.
 *
 * Usage (CI `RUNNER_TEMP` only, values never echoed):
 *   node e2e/scripts/prepare-real-nzb.mjs --input "$RUNNER_TEMP/test.nzb" \
 *     --output "$RUNNER_TEMP/staging-real.nzb"
 */

import { readFileSync, writeFileSync } from 'node:fs';

/** Largest accepted input: NZBs are XML metadata, never content (mirrors the intake guard). */
const MAX_INPUT_BYTES = 2 * 1024 * 1024;

function usage() {
  return (
    'usage: prepare-real-nzb.mjs --input <full.nzb> --output <staging.nzb>\n' +
    'Writes the first non-PAR2 <file> block (verbatim, all segments) as a single-file NZB.'
  );
}

function parseArgs(argv) {
  const out = { input: null, output: null };
  for (let i = 0; i < argv.length; i += 1) {
    if (argv[i] === '--input' && i + 1 < argv.length) out.input = argv[(i += 1)];
    else if (argv[i] === '--output' && i + 1 < argv.length) out.output = argv[(i += 1)];
    else if (argv[i] === '--help' || argv[i] === '-h') {
      process.stdout.write(`${usage()}\n`);
      process.exit(0);
    } else {
      process.stderr.write(`unknown argument: ${argv[i]}\n${usage()}\n`);
      process.exit(2);
    }
  }
  if (!out.input || !out.output) {
    process.stderr.write(`missing --input/--output\n${usage()}\n`);
    process.exit(2);
  }
  return out;
}

/** Split the document into its `<head>` block and ordered `<file>` blocks. */
function splitNzb(xml) {
  const head = xml.match(/<head>[\s\S]*?<\/head>/);
  const files = xml.match(/<file\b[\s\S]*?<\/file>/g) ?? [];
  return { head: head ? head[0] : null, files };
}

/**
 * True when a `<file>` block is a PAR2 file: a `.par2` extension appears in
 * its subject value (case-insensitive). The subject region is sliced from
 * `subject=` to the end of the opening tag, so inner-quoted indexer file
 * names (`... - "name.par2" yEnc`) match as well as bare ones
 * (`name.par2`); a naive `subject="([^"]*)"` capture stops at the first
 * inner quote and would miss both. Extension check only — the name itself
 * is never extracted or printed.
 */
function isPar2File(fileBlock) {
  const tag = fileBlock.match(/<file\b[\s\S]*?>/);
  if (!tag || !tag[0].includes('subject=')) return false;
  const region = tag[0].slice(tag[0].indexOf('subject='));
  return /\.par2(?=["'\s()\]]|$)/i.test(region);
}

function main() {
  const { input, output } = parseArgs(process.argv.slice(2));
  let xml;
  try {
    const buf = readFileSync(input);
    if (buf.length === 0 || buf.length > MAX_INPUT_BYTES) {
      process.stderr.write(
        `refusing input outside the byte cap (size=${buf.length}, cap=${MAX_INPUT_BYTES}): ` +
          'names only — no content read.\n',
      );
      process.exit(1);
    }
    xml = buf.toString('utf8');
  } catch (err) {
    process.stderr.write(`cannot read input (${err.code ?? 'read error'}): names only.\n`);
    process.exit(1);
  }
  const { head, files } = splitNzb(xml);
  if (!head || files.length === 0) {
    process.stderr.write(
      `no NZB <file> blocks found (files=${files.length}, head=${head ? 'present' : 'absent'}).\n`,
    );
    process.exit(1);
  }
  const dataFile = files.find((block) => !isPar2File(block)) ?? files[0];
  const segments = dataFile.match(/<segment\b[^>]*>[\s\S]*?<\/segment>/g) ?? [];
  if (segments.length === 0) {
    process.stderr.write('chosen file block carries no segments; refusing an empty fixture.\n');
    process.exit(1);
  }
  const doc =
    '<?xml version="1.0" encoding="UTF-8"?>\n<nzb xmlns="http://www.newzbin.com/DTD/2003/nzb">\n' +
    `  ${head}\n  ${dataFile}\n</nzb>\n`;
  try {
    writeFileSync(output, doc);
  } catch (err) {
    process.stderr.write(`cannot write output (${err.code ?? 'write error'}): names only.\n`);
    process.exit(1);
  }
  // Counts only — never IDs, subjects, file names or segment lists.
  process.stdout.write(`prepared staging NZB: input files=${files.length}, output files=1\n`);
}

main();
