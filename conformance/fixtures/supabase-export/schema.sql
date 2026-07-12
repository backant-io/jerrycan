-- Reference Supabase export: a multi-tenant CRM ("acme-crm").
-- Every translator shape the eval pins is exercised here.

create table public.workspaces (
    id uuid primary key,
    name text not null
);

create table public.workspace_members (
    workspace_id uuid not null references public.workspaces(id) on delete cascade,
    user_id uuid not null,
    role text not null check (role in ('owner', 'member')),
    primary key (workspace_id, user_id)
);

create table public.customers (
    id uuid primary key,
    workspace_id uuid not null references public.workspaces(id) on delete cascade,
    email text not null unique,
    name text not null
);

create table public.notes (
    id uuid primary key,
    workspace_id uuid not null references public.workspaces(id) on delete cascade,
    body text not null
);

create table public.plans (
    id uuid primary key,
    name text not null,
    price integer not null
);

create table public.events (
    id uuid primary key,
    kind text not null,
    created_at timestamptz not null
);

-- Tenant isolation via membership-join RLS.
alter table public.customers enable row level security;
create policy customers_tenant on public.customers using
    (workspace_id in (select workspace_id from public.workspace_members where user_id = auth.uid()));
create policy customers_delete on public.customers for delete using
    (workspace_id in (select workspace_id from public.workspace_members where user_id = auth.uid() and role = 'owner'));

alter table public.notes enable row level security;
create policy notes_tenant on public.notes using
    (workspace_id in (select workspace_id from public.workspace_members where user_id = auth.uid()));
-- A share-list join: NOT a canonical shape → MUST gap (never guessed).
create policy notes_shared on public.notes for select using
    (exists (select 1 from public.note_shares s where s.note_id = notes.id and s.shared_with = auth.uid()));

-- Public catalog.
alter table public.plans enable row level security;
create policy plans_public on public.plans for select using (true);

-- Storage bucket policies (storage.objects).
create policy avatar_owner on storage.objects for all using
    (bucket_id = 'avatars' and (storage.foldername(name))[1] = auth.uid()::text);
create policy invoice_shares on storage.objects for select using
    (bucket_id = 'invoices' and exists
        (select 1 from public.invoice_shares s where s.object_id = objects.id and s.user_id = auth.uid()));

-- plpgsql function + trigger (bodies are agent work → gap items).
create function public.touch() returns trigger as $$
begin
    new.updated_at := now();
    return new;
end;
$$ language plpgsql;

create trigger notes_touch before update on public.notes
    for each row execute function public.touch();

-- Realtime: customers + notes stream row changes.
create publication supabase_realtime for table public.customers, public.notes;
