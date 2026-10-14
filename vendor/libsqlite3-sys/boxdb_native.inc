/* Shared native-value dispatch. Persistent cells remain ordinary BLOBs. */
/* Reject removed declarations and CAST targets before affinity can erase the
** spelling. Whitespace and optional parameters do not restore an alias. */
static int boxdbRemovedType(const char *z, int length){
  char name[24]; int i,n=0;
  for(i=0;i<length && z[i]!='(';i++){
    if( sqlite3Isspace(z[i]) ) continue;
    if( n>=(int)sizeof(name)-1 ) return 0;
    name[n++]=sqlite3Toupper(z[i]);
  }
  name[n]=0;
  return strcmp(name,"DOUBLEPRECISION")==0
      || strcmp(name,"CHARACTERVARYING")==0
      || strcmp(name,"JSONB")==0
      || strcmp(name,"FLOAT4")==0
      || strcmp(name,"FLOAT")==0
      || strcmp(name,"FLOAT8")==0;
}

typedef struct BoxdbType { int kind; u8 affinity; char tag; int size; const char *name; } BoxdbType;
#include "boxdb_types.inc"
extern int boxdb_native_text(int,const char*,int,char**,int*);
extern int boxdb_native_eval(int,int,int,const char*,int,int,const char*,int,char**,int*,int*);
extern int boxdb_native_compare(int,const char*,int,int,const char*,int);
extern sqlite3_uint64 boxdb_native_hash(int,const char*,int);

static const BoxdbType *boxdbType(int kind){
  unsigned i;
  for(i=0; i<ArraySize(boxdbTypes); i++) if( boxdbTypes[i].kind==kind ) return &boxdbTypes[i];
  return 0;
}
static int boxdbNativeKind(const Mem *p){
  unsigned i;
  if( (p->flags&(MEM_Blob|MEM_Str))!=MEM_Blob || p->n<1 || !p->z ) return 0;
  for(i=1; i<ArraySize(boxdbTypes); i++){
    if( p->z[0]==boxdbTypes[i].tag && p->n==1+boxdbTypes[i].size ) return boxdbTypes[i].kind;
  }
  return 0;
}
static int boxdbKind(const Mem *p){
  int kind;
  if( !(p->flags&MEM_Blob) ) return 0;
  kind=boxdbNativeKind(p);
  return kind ? kind : boxdbIsDecimal(p);
}
static int boxdbNativeAffinityKind(u8 affinity){
  if( affinity<boxdbTypes[1].affinity || affinity>boxdbTypes[ArraySize(boxdbTypes)-1].affinity ) return 0;
  return boxdbTypes[affinity-boxdbTypes[0].affinity].kind;
}
static int boxdbNativeText(Mem *p){
  char *text=0; int length=0;
  int rc=boxdb_native_text(boxdbNativeKind(p),p->z+1,p->n-1,&text,&length);
  if( rc ){ sqlite3_free(text); return rc; }
  return sqlite3VdbeMemSetStr(p,text,length,SQLITE_UTF8,sqlite3_free);
}
static int boxdbNativeOperand(Mem *p, const char **data, int *length){
  int kind=boxdbKind(p);
  if( kind ){ *data=p->z+1; *length=p->n-1; return kind; }
  if( p->flags&(MEM_Int|MEM_IntReal) ){ *data=(const char*)&p->u.i; *length=8; return -1; }
  if( p->flags&MEM_Real ){ *data=(const char*)&p->u.r; *length=8; return -2; }
  if( p->flags&MEM_Str ){ *data=(const char*)sqlite3_value_text(p); *length=sqlite3_value_bytes(p); return 0; }
  *data=""; *length=0; return -3;
}
typedef struct BoxdbFunctionArgument { int kind; const char *data; int length; } BoxdbFunctionArgument;
extern int boxdb_native_function(int,int,const BoxdbFunctionArgument*,char**,int*);
static void boxdbTemporalFunction(sqlite3_context *context, int argc, sqlite3_value **argv){
  BoxdbFunctionArgument args[7];
  char *result=0; int i,length=0,rc,function=(int)(intptr_t)sqlite3_user_data(context);
  i64 currentSeconds;
  if( argc>7 ){ sqlite3_result_error(context,"temporal function accepts at most seven arguments",-1); return; }
  for(i=0;i<argc;i++){
    if( sqlite3_value_type(argv[i])==SQLITE_NULL ) return;
    args[i].kind=boxdbNativeOperand(argv[i],&args[i].data,&args[i].length);
    if( !args[i].data ){ sqlite3_result_error_nomem(context); return; }
  }
  if( function==5 ){
    i64 current=sqlite3StmtCurrentTime(context);
    if( current<=0 ){ sqlite3_result_error(context,"cannot read current UTC date",-1); return; }
    currentSeconds=(current-210866760000000LL)/1000;
    args[argc].kind=-1; args[argc].data=(const char*)&currentSeconds; args[argc].length=8;
    argc++;
  }
  rc=boxdb_native_function(function,argc,args,&result,&length);
  if( rc==SQLITE_OK ){
    rc=sqlite3VdbeMemSetStr(context->pOut,result,length,0,sqlite3_free);
    if( rc ) sqlite3_result_error_nomem(context);
  }else{
    if( rc==SQLITE_NOMEM ) sqlite3_result_error_nomem(context);
    else sqlite3_result_error(context,result ? result : "temporal function failed",length);
    sqlite3_free(result);
  }
}
static int boxdbNativeOp(Mem *out, int operation, int target, Mem *a, Mem *b, char **error){
  const char *za,*zb=""; int na,nb=0,ka,kb=-3,nresult=0,rc,resultKind=0; char *result=0;
  ka=boxdbNativeOperand(a,&za,&na);
  if( b ) kb=boxdbNativeOperand(b,&zb,&nb);
  if( !za || !zb ) return SQLITE_NOMEM;
  rc=boxdb_native_eval(operation,target,ka,za,na,kb,zb,nb,&result,&nresult,&resultKind);
  if( rc==SQLITE_OK && resultKind==-1 ){ i64 value; if( nresult!=8 ){ sqlite3_free(result); return SQLITE_ERROR; } memcpy(&value,result,8); sqlite3_free(result); sqlite3VdbeMemSetInt64(out,value); return SQLITE_OK; }
  if( rc==SQLITE_OK ) return sqlite3VdbeMemSetStr(out,result,nresult,0,sqlite3_free);
  if( error ) *error=result; else sqlite3_free(result);
  return rc;
}
static int boxdbNativeCoerce(Mem *p, int kind){
  if( (p->flags&MEM_Null) || boxdbNativeKind(p)==kind ) return SQLITE_OK;
  return boxdbNativeOp(p,8,kind,p,0,0);
}
static int boxdbNativeCompare(const Mem *a, const Mem *b){
  return boxdb_native_compare(boxdbNativeKind(a),a->z+1,a->n-1,boxdbNativeKind(b),b->z+1,b->n-1);
}
static int boxdbRank(const Mem *p){
  int kind=boxdbKind(p);
  if( kind ) return kind==1 ? 1 : (kind==5 ? 5 : kind+1);
  if( p->flags&(MEM_Int|MEM_IntReal|MEM_Real) ) return 1;
  return p->flags&MEM_Str ? 100 : 101;
}
static int boxdbNativeAffinityCompare(Mem *left, Mem *right, CollSeq *coll, u8 affinity){
  Mem a,b; int rc,kind=boxdbNativeAffinityKind(affinity);
  memset(&a,0,sizeof(a)); memset(&b,0,sizeof(b)); a.db=left->db; b.db=right->db;
  sqlite3VdbeMemShallowCopy(&a,left,MEM_Ephem); sqlite3VdbeMemShallowCopy(&b,right,MEM_Ephem);
  if( kind ){
    boxdbNativeCoerce(&a,kind); boxdbNativeCoerce(&b,kind);
  }else if( affinity==SQLITE_AFF_TEXT ){
    if( boxdbNativeKind(&a) ) boxdbNativeText(&a);
    if( boxdbNativeKind(&b) ) boxdbNativeText(&b);
  }
  rc=sqlite3MemCompare(&a,&b,coll);
  sqlite3VdbeMemRelease(&a); sqlite3VdbeMemRelease(&b);
  return rc;
}
