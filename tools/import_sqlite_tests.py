#!/usr/bin/env python3
"""Extract explicit, named SQLite cases. Originals and expected answers remain unchanged."""
import hashlib, json, pathlib, re, subprocess
ROOT = pathlib.Path(__file__).resolve().parents[1]
UPSTREAM = ROOT / 'tests/sqlite/upstream'
REVISION = 'be8d059e9a49089ab2dce5ed26dd87aaf598fdc5fbe0b107c0dd758464bcd5e3'

def brace(text, start):
    assert text[start] == '{'
    depth, i = 1, start + 1
    while depth:
        if text[i] == '\\':
            i += 2
            continue
        depth += (text[i] == '{') - (text[i] == '}')
        i += 1
    return text[start+1:i-1], i

def tcl_list(value):
    encoded = value.encode().hex()
    script = f'set value [encoding convertfrom utf-8 [binary format H* {{{encoded}}}]]\nforeach item $value {{binary scan [encoding convertto utf-8 $item] H* hex; puts $hex}}\n'
    result = subprocess.run(['tclsh'], input=script, text=True, capture_output=True, check=True)
    if result.stderr:
        raise ValueError(result.stderr)
    return [bytes.fromhex(line).decode() for line in result.stdout.splitlines()]

def tests(file):
    text = (UPSTREAM / file).read_text()
    result = {}
    for match in re.finditer(r'\bdo_(?:execsql_)?test\s+([\w.-]+)\s+\{', text):
        body, end = brace(text, match.end()-1)
        start = end
        while text[start:start+1].isspace(): start += 1
        if text[start:start+1] != '{': continue
        expected, _ = brace(text, start)
        sql = []
        if match.group().startswith('do_execsql_test'): sql = [body]
        else:
            for sql_match in re.finditer(r'\b(?:execsql|catchsql)\s+\{', body):
                query, _ = brace(body, sql_match.end()-1)
                sql.append(query)
        result[match[1]] = {'id':match[1], 'file':file, 'line':text[:match.start()].count('\n')+1, 'sql': ';\n'.join(q.strip().rstrip(';') for q in sql)+';', 'expected':tcl_list(expected), 'caught': 'catchsql' in body}
    return result

def case(source, setup='', sql=None, affected=None, adaptations=None):
    result = dict(source, setup=setup, sql=source['sql'] if sql is None else sql, affected=affected, adaptations=adaptations or [], error=None)
    if source.get('caught'):
        result['original_expected'] = source['expected']
        if source['expected'][0] == '1':
            message = source['expected'][1]
            result['error'] = 'UNIQUE' if 'UNIQUE' in message else 'CHECK' if 'CHECK' in message else 'primary key' if 'primary key' in message else message
            result['expected'] = []
        else: result['expected'] = tcl_list(source['expected'][1])
        result['adaptations'].append('catchsql status checked through Result; errors compared by category')
    return result

groups = {}
null = tests('null.test')
null_setup = null['null-1.0']['sql'].replace('create table t1(a,b,c)', 'create table t1(a INTEGER,b INTEGER,c INTEGER)')
groups['null'] = [case(c, '' if key=='null-1.0' else null_setup, sql=null_setup if key=='null-1.0' else None, adaptations=['Explicit INTEGER column types in setup']) for key,c in null.items() if re.fullmatch(r'null-[1-5]\.\d+',key) or key in ('null-6.1','null-6.2','null-6.3')]
select = tests('select1.test')
select_setup = "CREATE TABLE test1(f1 INT,f2 INT); INSERT INTO test1 VALUES(11,22); CREATE TABLE test2(r1 DOUBLE PRECISION,r2 DOUBLE PRECISION); INSERT INTO test2 VALUES(1.1,2.2);"
groups['select'] = [case(c,select_setup,adaptations=['REAL setup columns widened to DOUBLE PRECISION']) for key,c in select.items() if re.fullmatch(r'select1-1\.(?:[4-9](?:\.[1-3])?|1[0-3](?:\.[12])?)',key)]
expr = (UPSTREAM/'expr.test').read_text()
expr_setup = "CREATE TABLE test1(i1 BIGINT,i2 BIGINT,r1 DOUBLE PRECISION,r2 DOUBLE PRECISION,t1 TEXT,t2 TEXT); INSERT INTO test1 VALUES(1,2,1.1,2.2,'hello','world');"
groups['expr'] = []
for line_number,line in enumerate(expr.splitlines(),1):
    if not re.match(r'\s*test_expr expr-1\.',line): continue
    parts = tcl_list(line.strip())
    if len(parts)!=5: continue
    _,id,settings,query,expected = parts
    number = int(re.match(r'expr-1\.(\d+)',id)[1])
    if number>99: continue
    groups['expr'].append({'id':id,'file':'expr.test','line':line_number,'setup':expr_setup,'sql':f'BEGIN; UPDATE test1 SET {settings}; SELECT {query} FROM test1; ROLLBACK;', 'expected':tcl_list(expected),'affected':None,'adaptations':['INT setup columns widened to BIGINT for original 64-bit cases','REAL setup columns widened to DOUBLE PRECISION']})
update = tests('update.test')
base = 'CREATE TABLE test1(f1 INT,f2 INT);' + ''.join(f'INSERT INTO test1 VALUES({i},{1<<i});' for i in range(1,11))
groups['update'] = []
for id in ['update-3.5','update-3.6','update-3.7','update-3.8','update-3.9','update-3.10','update-3.11','update-3.12','update-3.13','update-3.14','update-4.0','update-4.1','update-4.2','update-4.3','update-4.4','update-4.5','update-4.6','update-4.7']:
    c = update[id]; sql = re.sub(r'PRAGMA count_changes=(on|off)\s*;?', '', c['sql'], flags=re.I)
    affected = int(c['expected'][0]) if id in ('update-3.7','update-3.9','update-3.11','update-4.6') else None
    groups['update'].append(case(c,base,sql,affected,['Tcl setup loop expanded to the same ten SQL INSERTs','PRAGMA count_changes answers checked through QueryResult.affected'])); base += sql
insert = tests('insert.test'); groups['insert'] = []
base = 'CREATE TABLE test1(one INT,two INT,three INT);'
for id in ['insert-1.5','insert-1.5b','insert-1.5c','insert-1.6','insert-1.6b','insert-1.6c']:
    c = insert[id]; groups['insert'].append(case(c,base)); base += c['sql']
base = ''
for id in ['insert-2.1','insert-2.2','insert-2.3','insert-2.4','insert-2.10','insert-2.11','insert-2.12']:
    c = insert[id]; sql = c['sql'].replace('f4 int default 7.89','f4 NUMERIC default 7.89').replace('text default hi',"text default 'hi'").replace('f2 real','f2 DOUBLE PRECISION')
    groups['insert'].append(case(c,base,sql,adaptations=['Fractional default column explicitly NUMERIC','Bare TEXT default quoted','REAL setup column widened to DOUBLE PRECISION'])); base += sql
c = insert['insert-4.1']; groups['insert'].append(case(c,sql=c['sql'].replace('t3(a,b,c)','t3(a INT,b INT,c INT)'),adaptations=['Explicit INT column types']))
trans = tests('trans.test'); groups['transaction'] = [case(trans['trans-1.0']),case(trans['trans-1.1'])]
setup = trans['trans-1.0']['sql'] + trans['trans-1.1']['sql']
for id in ['trans-2.10','trans-3.1']: groups['transaction'].append(case(trans[id],setup))

def remove_pragmas(sql):
    return re.sub(r'PRAGMA[^;]*;', '', sql, flags=re.I)
unique = tests('unique.test'); groups['unique'] = []; base = ''
for id in ['unique-1.1','unique-1.1b'] + [f'unique-1.{i}' for i in range(2,9)] + [f'unique-2.{i}' for i in range(10)] + [f'unique-3.{i}' for i in range(1,5)] + [f'unique-4.{i}' for i in range(1,11)]:
    c = unique[id]; sql = c['sql'].replace('t4(a UNIQUE, b, c,','t4(a INT UNIQUE, b INT, c INT,')
    if id.startswith('unique-1.'):
        sql = re.sub(r'VALUES\((\d+),(\d+),(\d+)\)', lambda m: f"VALUES({m[1]},{m[2]},'{m[3]}')", sql, flags=re.I)
    imported = case(c,base,sql,adaptations=['Explicit INT types in t4','Numeric values targeting TEXT are quoted'])
    groups['unique'].append(imported)
    if imported['error'] is None: base += sql
check = tests('check.test'); groups['check'] = []; base = ''
for id in [f'check-1.{i}' for i in range(1,18)]:
    c = check[id]; imported = case(c,base); groups['check'].append(imported)
    if imported['error'] is None: base += c['sql']
index = tests('index.test'); groups['index'] = []; base = ''
for id in [f'index-4.{i}' for i in range(1,14)]:
    c = index[id]; sql = c['sql']
    if id == 'index-4.1':
        sql = sql.replace('CREATE TABLE test1(cnt int, power int);', 'CREATE TABLE test1(cnt int, power int);' + ''.join(f'INSERT INTO test1 VALUES({i},{1<<i});' for i in range(1,20)))
    groups['index'].append(case(c,base,sql,adaptations=['Tcl loop expanded to its nineteen INSERTs'])); base += sql
null8 = 'CREATE TABLE t4(x INTEGER,y INTEGER); INSERT INTO t4 VALUES(1,11),(2,NULL);'
for id in ['null-8.1','null-8.2','null-8.3','null-8.4','null-8.5','null-8.11','null-8.12','null-8.13','null-8.14','null-8.15']:
    c = null[id]; sql = c['sql'].replace('t4(x,y)','t4(x INTEGER,y INTEGER)')
    setup = '' if id=='null-8.1' else null8
    if id in ['null-8.12','null-8.13','null-8.14','null-8.15']: setup += 'CREATE INDEX t4i1 ON t4(y);'
    groups['null'].append(case(c,setup,sql,adaptations=['Explicit INTEGER types in t4']))
delete = tests('delete.test'); groups['delete'] = []; base = ''
for id in ['delete-3.1.1','delete-3.1.2','delete-3.1.3','delete-3.1.4','delete-3.1.5','delete-3.1.6.1','delete-3.1.7','delete-5.1.1','delete-5.1.2']:
    c = delete[id]; sql = remove_pragmas(re.sub(r'EXPLAIN[^;]*;', '', c['sql'], flags=re.I))
    affected = int(c['expected'][0]) if id in ('delete-3.1.4','delete-3.1.6.1','delete-5.1.1') else None
    groups['delete'].append(case(c,base,sql,affected,['PRAGMA count_changes checked via QueryResult.affected','EXPLAIN diagnostic removed'])); base += sql
insert2 = tests('insert2.test'); groups['insert_select'] = []; base = ''
for id in ['insert2-1.0','insert2-1.1.1','insert2-1.1.3','insert2-1.2.1','insert2-1.2.2','insert2-1.3.1','insert2-1.3.2','insert2-2.0','insert2-2.1','insert2-2.2']:
    c = insert2[id]; sql = remove_pragmas(re.sub(r'EXPLAIN[^;]*;', '', c['sql'], flags=re.I))
    if id=='insert2-1.0': sql = sql.replace('CREATE TABLE d1(n int, log int);','CREATE TABLE d1(n int, log int);' + ''.join(f'INSERT INTO d1 VALUES({i},{(i-1).bit_length()});' for i in range(1,21)))
    sql = sql.replace('t3(a,b,c)','t3(a INT,b INT,c INT)').replace('t4(x,y)','t4(x INT,y INT)')
    affected = int(c['expected'][0]) if id in ('insert2-1.1.1','insert2-1.2.1') else None
    groups['insert_select'].append(case(c,base,sql,affected,['Tcl loop expanded to its twenty INSERTs','Explicit INT column types','PRAGMA count_changes checked via QueryResult.affected','EXPLAIN diagnostic removed'])); base += sql
fkey = tests('fkey1.test'); groups['foreign_key'] = []
for id in ['fkey1-1.1','fkey1-2.1','fkey1-4.0','fkey1-4.1']:
    c = fkey[id]; sql = remove_pragmas(c['sql']); sql = re.sub(r'(x) references t4',r'\1 INTEGER references t4',sql)
    groups['foreign_key'].append(case(c,sql=sql,adaptations=['Foreign keys always enabled; PRAGMA removed','Explicit INTEGER types for referencing columns']))

# Cases are explicitly selected above; no selection based on BoxDB results.
(ROOT/'tests/sqlite/cases.json').write_text(json.dumps(groups,ensure_ascii=False,indent=2)+'\n')
manifest = {'sqlite_version':'3.54.0','revision':REVISION,'source':'https://sqlite.org/src','files':{p.name:{'sha256':hashlib.sha256(p.read_bytes()).hexdigest(),'url':f'https://sqlite.org/src/raw/test/{p.name}?ci={REVISION}'} for p in sorted(UPSTREAM.glob('*.test'))},'adapted_cases':{group:len(cases) for group,cases in groups.items()},'not_ported':'All cases not explicitly listed in cases.json. This is not the complete SQLite Tcl/C test harness.'}
(ROOT/'tests/sqlite/provenance.json').write_text(json.dumps(manifest,indent=2)+'\n')
print(json.dumps(manifest['adapted_cases']))
