/* Native BoxDB decimals retain SQLite's BLOB record encoding and C type ABI. */
#include <float.h>
typedef struct BoxdbWireBlob { const char *data; int length; } BoxdbWireBlob;
#define BOXDB_DECIMAL_PREFIX "\000BDd1"
#define BOXDB_DECIMAL_PREFIX_LEN 5
#define BOXDB_BLOB_PREFIX "\000BDb1"
/* The spare Mem bit records an API text view without changing the wire value.
** u.nZero caches its byte length (negative for UTF-16); MEM_Zero is absent. */
#define MEM_BoxdbText 0x0080
static int boxdbNativeKind(const Mem*);
static int boxdbKind(const Mem*);
extern int boxdb_native_text(int,const char*,int,char**,int*);
extern int boxdb_decimal_eval(int,int,const char*,int,int,const char*,int,char**,int*);
extern int boxdb_decimal_compare(int,const char*,int,int,const char*,int);

static int boxdbIsDecimal(const Mem *p){
  int i, digits=0, fraction=0, dot=0, first=-1, last=-1, scale;
  if( !(p->flags & MEM_Blob) || p->n<=5 || p->n>8199 || !p->z
      || memcmp(p->z,BOXDB_DECIMAL_PREFIX,5)!=0 ) return 0;
  i=5;
  if( p->z[i]=='-' || p->z[i]=='+' ) i++;
  for(; i<p->n; i++){
    char c=p->z[i];
    if( c=='.' && !dot ){ dot=1; continue; }
    if( c<'0' || c>'9' ) return 0;
    if( c!='0' ){ if( first<0 ) first=digits; last=digits; }
    digits++;
    fraction+=dot;
  }
  if( !digits ) return 0;
  if( first<0 ) return 1;
  scale=fraction-(digits-last-1);
  return last-first+1<=4096 && scale>=-4096 && scale<=4096;
}

static int boxdbIsEscapedBlob(const Mem *p){
  return (p->flags & MEM_Blob)!=0 && p->n>=8 && p->z!=0
      && memcmp(p->z, BOXDB_BLOB_PREFIX, 5)==0 && memcmp(p->z+5, "\000BD",3)==0;
}

static int boxdbEscapeBlob(Mem *p){
  char *bytes;
  int n = p->n;
  if( n<3 || memcmp(p->z,"\000BD",3)!=0 ) return SQLITE_OK;
  bytes = sqlite3_malloc64((sqlite3_uint64)n+5);
  if( !bytes ) return SQLITE_NOMEM;
  memcpy(bytes,BOXDB_BLOB_PREFIX,5);
  memcpy(bytes+5,p->z,n);
  return sqlite3VdbeMemSetStr(p,bytes,n+5,0,sqlite3_free);
}

static const void *boxdbValueText(Mem *p, u8 enc){
  Mem text;
  u8 requested = enc & ~SQLITE_UTF16_ALIGNED;
  i64 offset, size;
  int n;
  memset(&text,0,sizeof(text));
  text.db=p->db;
  if( boxdbNativeKind(p) ){
    char *formatted=0; int length=0;
    if( boxdb_native_text(boxdbNativeKind(p),p->z+5,p->n-5,&formatted,&length) ){
      sqlite3_free(formatted); return 0;
    }
    sqlite3VdbeMemSetStr(&text,formatted,length,SQLITE_UTF8,sqlite3_free);
  }else{
    text.z=p->z+5; text.n=p->n-5;
    text.enc=boxdbIsDecimal(p) ? SQLITE_UTF8 : p->enc;
    text.flags=MEM_Str|MEM_Ephem;
  }
  if( sqlite3VdbeMemMakeWriteable(&text) ) return 0;
  if( sqlite3VdbeChangeEncoding(&text,requested) ){
    sqlite3VdbeMemRelease(&text);
    return 0;
  }
  n=text.n;
  offset=((i64)p->n+3)&~(i64)1;
  size=offset+n+2;
  if( size>0x7fffffff || sqlite3VdbeMemGrow(p,(int)size,1) ){
    sqlite3VdbeMemRelease(&text);
    return 0;
  }
  p->z[p->n]=p->z[p->n+1]=0;
  memcpy(p->z+offset,text.z,n);
  p->z[offset+n]=p->z[offset+n+1]=0;
  p->flags |= MEM_BoxdbText|MEM_Term;
  p->u.nZero=requested==SQLITE_UTF8 ? n : -n-1;
  sqlite3VdbeMemRelease(&text);
  return p->z+offset;
}

static int boxdbDecimalOperand(const Mem *p, const char **data, int *len, char *buf){
  if( boxdbIsDecimal(p) ){
    *data = p->z + BOXDB_DECIMAL_PREFIX_LEN;
    *len = p->n - BOXDB_DECIMAL_PREFIX_LEN;
    return 1;
  }
  if( p->flags & (MEM_Int|MEM_IntReal) ){
    sqlite3_snprintf(32, buf, "%lld", p->u.i);
    *data = buf;
    *len = (int)strlen(buf);
    return 2;
  }
  if( p->flags & MEM_Real ){
    *data = (const char*)&p->u.r;
    *len = sizeof(double);
    return 3;
  }
  if( p->flags & MEM_Str ){
    *data = (const char*)sqlite3_value_text((Mem*)p);
    *len = sqlite3_value_bytes((Mem*)p);
    return 1;
  }
  *data = "";
  *len = 0;
  return 1;
}

static int boxdbDecimalOp(Mem *out, int op, const Mem *a, const Mem *b, char **error){
  const char *za, *zb;
  char ba[32], bb[32], *result = 0;
  int na, nb, ka, kb, nresult = 0, rc;
  ka = boxdbDecimalOperand(a, &za, &na, ba);
  kb = boxdbDecimalOperand(b ? b : a, &zb, &nb, bb);
  if( !za || !zb ) return SQLITE_NOMEM;
  rc = boxdb_decimal_eval(op, ka, za, na, kb, zb, nb, &result, &nresult);
  if( rc==SQLITE_OK ){
    return sqlite3VdbeMemSetStr(out, result, nresult, 0, sqlite3_free);
  }
  if( error ) *error = result; else sqlite3_free(result);
  return rc;
}

static int boxdbDecimalCoerce(Mem *p){
  if( p->flags & MEM_Null || boxdbIsDecimal(p) ) return SQLITE_OK;
  if( p->flags & MEM_Str ){
    int rc = sqlite3VdbeChangeEncoding(p, SQLITE_UTF8);
    if( rc ) return rc;
  }
  return boxdbDecimalOp(p, 0, p, 0, 0);
}

static int boxdbDecimalText(Mem *p){
  char *text = sqlite3_mprintf("%.*s", p->n-BOXDB_DECIMAL_PREFIX_LEN,
                              p->z+BOXDB_DECIMAL_PREFIX_LEN);
  if( !text ) return SQLITE_NOMEM;
  return sqlite3VdbeMemSetStr(p, text, -1, SQLITE_UTF8, sqlite3_free);
}

static int boxdbBlobText(Mem *p, u8 enc){
  int rc;
  char *text=sqlite3_malloc64((sqlite3_uint64)p->n-5+2);
  if( !text ) return SQLITE_NOMEM;
  memcpy(text,p->z+5,p->n-5);
  text[p->n-5]=text[p->n-4]=0;
  rc=sqlite3VdbeMemSetStr(p,text,p->n-5,p->enc,sqlite3_free);
  if( rc==SQLITE_OK ) rc=sqlite3VdbeChangeEncoding(p,enc);
  return rc;
}

static int boxdbDecimalCompare(const Mem *a, const Mem *b){
  const char *za, *zb;
  char ba[32], bb[32];
  int na, nb, ka, kb;
  ka = boxdbDecimalOperand(a, &za, &na, ba);
  kb = boxdbDecimalOperand(b, &zb, &nb, bb);
  if( ka==3 && (a->u.r>DBL_MAX || a->u.r<-DBL_MAX) ) return a->u.r>0 ? 1 : -1;
  if( kb==3 && (b->u.r>DBL_MAX || b->u.r<-DBL_MAX) ) return b->u.r>0 ? -1 : 1;
  return boxdb_decimal_compare(ka, za, na, kb, zb, nb);
}

static int boxdbDecimalAffinityCompare(Mem *left, Mem *right, CollSeq *coll, char affinity){
  Mem a,b;
  int result;
  memset(&a,0,sizeof(a));
  memset(&b,0,sizeof(b));
  a.db=left->db;
  b.db=right->db;
  sqlite3VdbeMemShallowCopy(&a,left,MEM_Ephem);
  sqlite3VdbeMemShallowCopy(&b,right,MEM_Ephem);
  if( affinity==SQLITE_AFF_DECIMAL ){
    boxdbDecimalCoerce(&a);
    boxdbDecimalCoerce(&b);
  }else{
    if( boxdbIsDecimal(&a) && boxdbDecimalText(&a)==SQLITE_OK ) sqlite3VdbeChangeEncoding(&a,ENC(a.db));
    if( boxdbIsDecimal(&b) && boxdbDecimalText(&b)==SQLITE_OK ) sqlite3VdbeChangeEncoding(&b,ENC(b.db));
  }
  result=sqlite3MemCompare(&a,&b,coll);
  sqlite3VdbeMemRelease(&a);
  sqlite3VdbeMemRelease(&b);
  return result;
}

static void boxdbDecimalAffinity(Mem *p, char affinity, u8 enc){
  Mem candidate;
  if( affinity==SQLITE_AFF_TEXT ){
    if( boxdbDecimalText(p)==SQLITE_OK ) sqlite3VdbeChangeEncoding(p,enc);
    return;
  }
  if( affinity<SQLITE_AFF_NUMERIC ) return;
  memset(&candidate,0,sizeof(candidate));
  candidate.flags=MEM_Int;
  candidate.u.i=boxdbDecimalInt(p);
  if( boxdbDecimalCompare(p,&candidate)==0 ){
    sqlite3VdbeMemSetInt64(p,candidate.u.i);
    return;
  }
  candidate.flags=MEM_Real;
  candidate.u.r=boxdbDecimalReal(p);
  if( boxdbDecimalCompare(p,&candidate)==0 ) sqlite3VdbeMemSetDouble(p,candidate.u.r);
}

static i64 boxdbDecimalInt(const Mem *p){
  Mem text = *p;
  text.z += BOXDB_DECIMAL_PREFIX_LEN;
  text.n -= BOXDB_DECIMAL_PREFIX_LEN;
  text.flags = MEM_Str|MEM_Ephem;
  text.enc = SQLITE_UTF8;
  return sqlite3VdbeIntValue(&text);
}

static double boxdbDecimalReal(Mem *p){
  Mem text = *p;
  text.z += BOXDB_DECIMAL_PREFIX_LEN;
  text.n -= BOXDB_DECIMAL_PREFIX_LEN;
  text.flags = MEM_Str|MEM_Ephem;
  text.enc = SQLITE_UTF8;
  return sqlite3VdbeRealValue(&text);
}

static int boxdbDecimalBoolean(const Mem *p){
  int i;
  for(i=BOXDB_DECIMAL_PREFIX_LEN; i<p->n; i++){
    if( p->z[i]>='1' && p->z[i]<='9' ) return 1;
  }
  return 0;
}

static void boxdbDecimalFunction(sqlite3_context *ctx, int op, Mem *a, Mem *b){
  Mem result;
  char *error = 0;
  memset(&result, 0, sizeof(result));
  result.db = sqlite3_context_db_handle(ctx);
  if( boxdbDecimalOp(&result, op, a, b, &error) ){
    sqlite3_result_error(ctx, error ? error : "DECIMAL allocation failed", -1);
    sqlite3_free(error);
  }else{
    sqlite3_result_value(ctx, &result);
  }
  sqlite3VdbeMemRelease(&result);
}

/* Stored DECIMAL precision checks belong to the engine, including SQL imports. */
static void boxdbDecimalValidFunc(sqlite3_context *ctx, int argc, sqlite3_value **argv){
  Mem *value=(Mem*)argv[0];
  i64 precision, scale;
  int i, whole=0, fraction=0, significantFraction=0, dot=0, leading=1;
  UNUSED_PARAMETER(argc);
  if( value->flags & MEM_Null ){ sqlite3_result_int(ctx,1); return; }
  precision=sqlite3_value_int64(argv[1]);
  scale=sqlite3_value_int64(argv[2]);
  if( sqlite3_value_type(argv[1])!=SQLITE_INTEGER || sqlite3_value_type(argv[2])!=SQLITE_INTEGER
      || precision<1 || precision>1000 || scale<0 || scale>precision ){
    sqlite3_result_error(ctx,"invalid DECIMAL precision/scale",-1);
    return;
  }
  if( !boxdbIsDecimal(value) ){
    sqlite3_result_error(ctx,"invalid DECIMAL value",-1);
    return;
  }
  i=5;
  if( value->z[i]=='-' || value->z[i]=='+' ) i++;
  for(; i<value->n; i++){
    char c=value->z[i];
    if( c=='.' ){ dot=1; continue; }
    if( dot ){
      fraction++;
      if( c!='0' ) significantFraction=fraction;
    }else{
      if( c!='0' ) leading=0;
      if( !leading ) whole++;
    }
  }
  if( whole>precision-scale || significantFraction>scale ){
    sqlite3_result_error(ctx,"DECIMAL precision or scale overflow",-1);
  }else sqlite3_result_int(ctx,1);
}

static int boxdbConcat(Mem *out, Mem *a, Mem *b, u8 enc){
  const char *za=(const char*)sqlite3_value_text(a);
  const char *zb=(const char*)sqlite3_value_text(b);
  int na=sqlite3_value_bytes(a), nb=sqlite3_value_bytes(b);
  char *text;
  int rc;
  if( !za || !zb ) return SQLITE_NOMEM;
  if( (i64)na+nb>out->db->aLimit[SQLITE_LIMIT_LENGTH] ) return SQLITE_TOOBIG;
  text=sqlite3_malloc64((sqlite3_uint64)na+nb+1);
  if( !text ) return SQLITE_NOMEM;
  memcpy(text,za,na);
  memcpy(text+na,zb,nb);
  text[na+nb]=0;
  rc=sqlite3VdbeMemSetStr(out,text,na+nb,SQLITE_UTF8,sqlite3_free);
  if( rc==SQLITE_OK ) rc=sqlite3VdbeChangeEncoding(out,enc);
  return rc;
}
