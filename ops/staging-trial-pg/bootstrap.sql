-- initdb requires a password for SCRAM initialization. Its socket-only
-- bootstrap role keeps no password once the temporary startup has completed.
ALTER ROLE nntune_trial_bootstrap PASSWORD NULL;
