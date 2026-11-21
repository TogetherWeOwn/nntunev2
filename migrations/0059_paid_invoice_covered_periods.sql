SET search_path = public, pg_catalog;

-- Exact half-open paid service coverage, independent of the reporting span.
-- NULL means unknown: the reported plan and span cannot reconstruct it.
ALTER TABLE paid_invoice_history
    ADD COLUMN IF NOT EXISTS covered_periods TSTZMULTIRANGE;
