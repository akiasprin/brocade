SELECT jsonb_build_object(
 'relations',(SELECT jsonb_agg(jsonb_build_object(
   'name',c.relname,'kind',c.relkind,'owner',c.relowner=current_user::regrole,
   'rls',c.relrowsecurity,'force_rls',c.relforcerowsecurity,'persistence',c.relpersistence,
   'columns',(SELECT jsonb_agg(jsonb_build_object('name',a.attname,
      'type',format_type(a.atttypid,a.atttypmod),
      'collation',CASE WHEN a.attcollation=0 THEN NULL ELSE a.attcollation::regcollation::text END,'not_null',a.attnotnull,'identity',a.attidentity,
      'generated',a.attgenerated,'default',pg_get_expr(d.adbin,d.adrelid)) ORDER BY a.attnum)
      FROM pg_attribute a LEFT JOIN pg_attrdef d ON d.adrelid=a.attrelid AND d.adnum=a.attnum
      WHERE a.attrelid=c.oid AND a.attnum>0 AND NOT a.attisdropped),
   'constraints',(SELECT jsonb_agg(jsonb_build_object('name',k.conname,
      'definition',pg_get_constraintdef(k.oid),'validated',k.convalidated) ORDER BY k.conname)
      FROM pg_constraint k WHERE k.conrelid=c.oid),
   'indexes',(SELECT jsonb_agg(jsonb_build_object('definition',pg_get_indexdef(i.indexrelid),
      'valid',i.indisvalid,'ready',i.indisready) ORDER BY pg_get_indexdef(i.indexrelid))
      FROM pg_index i WHERE i.indrelid=c.oid),
   'sequence',(SELECT jsonb_build_object('type',s.seqtypid::regtype::text,'start',s.seqstart,
      'increment',s.seqincrement,'min',s.seqmin,'max',s.seqmax,'cache',s.seqcache,'cycle',s.seqcycle)
      FROM pg_sequence s WHERE s.seqrelid=c.oid)) ORDER BY c.relname)
   FROM pg_class c WHERE c.relnamespace='public'::regnamespace AND (c.relname='user_clash_rules' OR c.relname LIKE 'ip\_maintenance\_%' ESCAPE '\')),
 'triggers',(SELECT jsonb_agg(jsonb_build_object('definition',pg_get_triggerdef(t.oid),
   'enabled',t.tgenabled) ORDER BY pg_get_triggerdef(t.oid)) FROM pg_trigger t
   JOIN pg_proc p ON p.oid=t.tgfoid WHERE NOT t.tgisinternal
     AND (t.tgrelid='public.user_clash_rules'::regclass OR t.tgname='agent_channel_ip_maintenance' OR
       t.tgrelid IN (SELECT oid FROM pg_class WHERE relnamespace='public'::regnamespace AND relname LIKE 'ip\_maintenance\_%' ESCAPE '\')))
)::text
