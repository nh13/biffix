// The backend's QC-preview test fixtures (written by make_fixtures.sh there), which the mock serves
// as they are: FastQC's and MultiQC's own data files, and a tiny BAM, BAI, FASTA and FAI.
export const FIXTURES = new URL('../../../crates/biffix-backend/tests/fixtures/', import.meta.url);
