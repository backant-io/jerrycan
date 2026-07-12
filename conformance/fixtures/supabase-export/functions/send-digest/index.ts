import { serve } from "https://deno.land/std/http/server.ts";
serve(() => new Response("digest sent"));
