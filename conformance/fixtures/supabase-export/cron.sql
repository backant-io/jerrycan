select cron.schedule('nightly-digest', '0 3 * * *', $$select public.send_digest()$$);
select cron.schedule('hourly-sync', '@hourly', $$select public.sync()$$);
