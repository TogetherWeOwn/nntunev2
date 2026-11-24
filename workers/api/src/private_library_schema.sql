-- Dormant private-release grants. Apply only through a separate protected task.
-- Invite and account removal also remove their access grants. Storage
-- cleanup is independent of these grants.
CREATE TABLE public.private_library_admissions (
    invite_id BIGINT NOT NULL REFERENCES public.invite_codes (id) ON DELETE CASCADE,
    recipient_id TEXT NOT NULL REFERENCES public.users (id) ON DELETE CASCADE,
    track_id UUID NOT NULL REFERENCES public.tracks (id) ON DELETE RESTRICT,
    accepted_seq BIGINT NOT NULL CHECK (accepted_seq > 0),
    object_id UUID NOT NULL,
    PRIMARY KEY (invite_id, track_id)
);
CREATE INDEX ON public.private_library_admissions (track_id);
CREATE INDEX ON public.private_library_admissions (recipient_id);
