//! Checked transformations of the pinned upstream amalgamation.
use std::{fs, path::PathBuf};
#[path = "native_types.rs"]
mod native_types;

pub fn source(out_dir: &str) -> PathBuf {
    println!("cargo:rerun-if-changed=boxdb.rs");
    println!("cargo:rerun-if-changed=boxdb_decimal.inc");
    println!("cargo:rerun-if-changed=boxdb_aggregate.inc");
    println!("cargo:rerun-if-changed=boxdb_native.inc");
    println!("cargo:rerun-if-changed=native_types.rs");
    let types = native_types::TYPES;
    for pair in types.windows(2) {
        assert_eq!(
            pair[1].affinity,
            pair[0].affinity + 1,
            "contiguous native affinities"
        );
    }
    assert!(
        types.last().unwrap().affinity <= 0x4f,
        "native affinities must fit comparison flags"
    );
    let descriptors = types
        .iter()
        .map(|t| {
            format!(
                "{{{},0x{:x},'{}',{},\"{}\"}}",
                t.kind as u8, t.affinity, t.tag as char, t.payload_size, t.name
            )
        })
        .collect::<Vec<_>>()
        .join(",\n");
    fs::write(
        PathBuf::from(out_dir).join("boxdb_types.inc"),
        format!("static const BoxdbType boxdbTypes[]={{ {descriptors} }};\n"),
    )
    .expect("native type descriptors");
    let mut source = fs::read_to_string("sqlite3/sqlite3.c").expect("pinned SQLite source");
    let mut replace = |old: &str, new: &str| {
        assert_eq!(source.matches(old).count(), 1, "SQLite patch anchor: {old}");
        source = source.replacen(old, new, 1);
    };
    replace(
        "#define SQLITE_AFF_DEFER",
        &format!(
            "{}\n#define SQLITE_AFF_DEFER",
            types
                .iter()
                .map(|t| format!("#define SQLITE_AFF_{} 0x{:x}", t.name, t.affinity))
                .collect::<Vec<_>>()
                .join("\n")
        ),
    );
    replace(
        "SQLITE_PRIVATE i64 sqlite3VdbeIntValue(const Mem*);",
        "SQLITE_PRIVATE i64 sqlite3VdbeIntValue(const Mem*);\nstatic int boxdbIsDecimal(const Mem*);\nstatic int boxdbNativeKind(const Mem*);\nstatic i64 boxdbDecimalInt(const Mem*);\nstatic double boxdbDecimalReal(Mem*);\nstatic int boxdbDecimalBoolean(const Mem*);",
    );
    replace(
        "SQLITE_PRIVATE i64 sqlite3VdbeIntValue(const Mem *pMem){\n  int flags;",
        "SQLITE_PRIVATE i64 sqlite3VdbeIntValue(const Mem *pMem){\n  int flags;\n  if( boxdbIsDecimal(pMem) ) return boxdbDecimalInt(pMem);",
    );
    replace(
        "SQLITE_PRIVATE double sqlite3VdbeRealValue(Mem *pMem){",
        "SQLITE_PRIVATE double sqlite3VdbeRealValue(Mem *pMem){\n  if( boxdbIsDecimal(pMem) ) return boxdbDecimalReal(pMem);",
    );
    replace(
        "SQLITE_PRIVATE int sqlite3VdbeBooleanValue(Mem *pMem, int ifNull){",
        "SQLITE_PRIVATE int sqlite3VdbeBooleanValue(Mem *pMem, int ifNull){\n  if( boxdbIsDecimal(pMem) ) return boxdbDecimalBoolean(pMem);",
    );
    replace(
        "static void absFunc(sqlite3_context *context, int argc, sqlite3_value **argv){",
        "static void absFunc(sqlite3_context *context, int argc, sqlite3_value **argv){\n  if( boxdbNativeKind(argv[0]) ){ sqlite3_result_error(context,\"abs requires a numeric value\",-1);return; }\n  if( boxdbIsDecimal((Mem*)argv[0]) ){ boxdbDecimalFunction(context,7,(Mem*)argv[0],0); return; }",
    );
    replace(
        "static void roundFunc(sqlite3_context *context, int argc, sqlite3_value **argv){",
        "static void roundFunc(sqlite3_context *context, int argc, sqlite3_value **argv){\n  if( boxdbNativeKind(argv[0]) ){ sqlite3_result_error(context,\"round requires a numeric value\",-1);return; }\n  if( boxdbIsDecimal((Mem*)argv[0]) ){\n    Mem scale;\n    if( argc==2 && sqlite3_value_type(argv[1])==SQLITE_NULL ) return;\n    memset(&scale,0,sizeof(scale));\n    scale.flags=MEM_Int;\n    scale.u.i=argc==2 ? sqlite3_value_int64(argv[1]) : 0;\n    if( scale.u.i<0 ) scale.u.i=0;\n    boxdbDecimalFunction(context,6,(Mem*)argv[0],&scale);\n    return;\n  }",
    );
    replace(
        "#define SQLITE_N_STDTYPE    6",
        &format!(
            "{}\n#define SQLITE_N_STDTYPE {}",
            types
                .iter()
                .enumerate()
                .map(|(i, t)| format!("#define COLTYPE_{} {}", t.name, 7 + i))
                .collect::<Vec<_>>()
                .join("\n"),
            6 + types.len()
        ),
    );
    replace(
        "sqlite3StdTypeLen[] = { 3, 4, 3, 7, 4, 4 };",
        &format!(
            "sqlite3StdTypeLen[] = {{ 3, 4, 3, 7, 4, 4, {} }};",
            types
                .iter()
                .map(|t| t.name.len().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    );
    replace(
        "  SQLITE_AFF_REAL,\n  SQLITE_AFF_TEXT\n};",
        &format!(
            "  SQLITE_AFF_REAL,\n  SQLITE_AFF_TEXT,\n{}\n}};",
            types
                .iter()
                .map(|t| format!("  SQLITE_AFF_{}", t.name))
                .collect::<Vec<_>>()
                .join(",\n")
        ),
    );
    replace(
        "  \"REAL\",\n  \"TEXT\"\n};",
        &format!(
            "  \"REAL\",\n  \"TEXT\",\n{}\n}};",
            types
                .iter()
                .map(|t| format!("  \"{}\"", t.name))
                .collect::<Vec<_>>()
                .join(",\n")
        ),
    );
    replace("      switch( aCol[i].eCType ){\n        case COLTYPE_BLOB:", &format!("      switch( aCol[i].eCType ){{\n{}\n        case COLTYPE_BLOB:",types.iter().map(|t|format!("        case COLTYPE_{}: {{ if( boxdbKind(pIn1)!={} ) goto vdbe_type_error; break; }}",t.name,t.kind as u8)).collect::<Vec<_>>().join("\n")));
    replace(
        "               0x14     /* TEXT */",
        &format!(
            "               0x14,    /* TEXT */\n{}",
            types
                .iter()
                .map(|_| "               0x18")
                .collect::<Vec<_>>()
                .join(",\n")
        ),
    );
    replace(
        "        /* SQLITE_AFF_FLEXNUM */ \" NUM\",",
        &format!(
            "        /* SQLITE_AFF_FLEXNUM */ \" NUM\",\n{}",
            types
                .iter()
                .map(|t| format!("        \" {}\",", t.name))
                .collect::<Vec<_>>()
                .join("\n")
        ),
    );
    replace(
        "SQLITE_PRIVATE int sqlite3VdbeMemCast(Mem *pMem, u8 aff, u8 encoding){",
        "#include \"boxdb_decimal.inc\"\n#include \"boxdb_native.inc\"\nSQLITE_PRIVATE int sqlite3VdbeMemCast(Mem *pMem, u8 aff, u8 encoding){",
    );
    replace(
        "  switch( aff ){\n    case SQLITE_AFF_BLOB:",
        "  if( boxdbNativeAffinityKind(aff) ) return (pMem->flags&MEM_Null) ? SQLITE_OK : boxdbNativeOp(pMem,0,boxdbNativeAffinityKind(aff),pMem,0,0);\n  if( aff==SQLITE_AFF_DECIMAL ){\n    if( pMem->flags & MEM_Real ) return boxdbDecimalOp(pMem,8,pMem,0,0);\n    return boxdbDecimalCoerce(pMem);\n  }\n  if( aff==SQLITE_AFF_BLOB && boxdbIsEscapedBlob(pMem) ) return SQLITE_OK;\n  if( boxdbIsEscapedBlob(pMem) ){\n    int rc=boxdbBlobText(pMem,encoding);\n    if( rc ) return rc;\n  }\n  if( boxdbNativeKind(pMem) && aff>=SQLITE_AFF_NUMERIC ) return SQLITE_ERROR;\n  if( boxdbKind(pMem) ){\n    int rc = boxdbNativeKind(pMem) ? boxdbNativeText(pMem) : boxdbDecimalText(pMem);\n    if( rc==SQLITE_OK ) rc=sqlite3VdbeChangeEncoding(pMem,encoding);\n    if( rc ) return rc;\n  }\n  switch( aff ){\n    case SQLITE_AFF_BLOB:",
    );
    replace(
        "        pMem->flags &= ~(MEM_TypeMask&~MEM_Blob);\n      }\n      break;",
        "        pMem->flags &= ~(MEM_TypeMask&~MEM_Blob);\n      }\n      return boxdbEscapeBlob(pMem);",
    );
    replace(
        "    sqlite3VdbeMemSetStr(pOut, pOp->p4.z, pOp->p1, 0, 0);\n  }\n  pOut->enc = encoding;",
        "    sqlite3VdbeMemSetStr(pOut, pOp->p4.z, pOp->p1, 0, 0);\n    if( boxdbEscapeBlob(pOut) ) goto no_mem;\n  }\n  pOut->enc = encoding;",
    );
    replace(
        "  /* Both values must be blobs.  Compare using memcmp().  */\n  return sqlite3BlobCompare(pMem1, pMem2);",
        "  /* Both values must be blobs.  Compare their unescaped payloads. */\n  if( boxdbIsEscapedBlob(pMem1) || boxdbIsEscapedBlob(pMem2) ){\n    Mem a=*pMem1, b=*pMem2;\n    if( boxdbIsEscapedBlob(&a) ){ a.z+=5; a.n-=5; }\n    if( boxdbIsEscapedBlob(&b) ){ b.z+=5; b.n-=5; }\n    return sqlite3BlobCompare(&a,&b);\n  }\n  return sqlite3BlobCompare(pMem1, pMem2);",
    );
    replace(
        "  assert( (pVal->flags & (MEM_Null))==0 );\n  if( pVal->flags & (MEM_Blob|MEM_Str) ){",
        "  assert( (pVal->flags & (MEM_Null))==0 );\n  if( boxdbKind(pVal) || boxdbIsEscapedBlob(pVal) ) return boxdbValueText(pVal,enc);\n  if( pVal->flags & (MEM_Blob|MEM_Str) ){",
    );
    replace(
        "SQLITE_API const void *sqlite3_value_blob(sqlite3_value *pVal){\n  Mem *p = (Mem*)pVal;",
        "SQLITE_API const void *sqlite3_value_blob(sqlite3_value *pVal){\n  Mem *p = (Mem*)pVal;\n  if( boxdbKind(p) || boxdbIsEscapedBlob(p) ){\n    p->flags |= MEM_BoxdbText;\n    p->u.nZero=p->n-5;\n    return p->z+5;\n  }",
    );
    replace(
        "SQLITE_API int sqlite3_value_bytes(sqlite3_value *pVal){",
        "SQLITE_API int sqlite3_value_bytes(sqlite3_value *pVal){\n  if( boxdbKind((Mem*)pVal) || boxdbIsEscapedBlob((Mem*)pVal) ){\n    if( (pVal->flags & MEM_BoxdbText)!=0 && pVal->u.nZero>=0 ) return pVal->u.nZero;\n    if( !boxdbValueText(pVal,SQLITE_UTF8) ) return 0;\n    return pVal->u.nZero;\n  }",
    );
    replace(
        "  val = sqlite3_value_blob( columnMem(pStmt,i) );",
        "  { Mem *m=columnMem(pStmt,i);\n    if( boxdbKind(m) || boxdbIsEscapedBlob(m) ){ m->flags &= ~MEM_BoxdbText; val=m->z; } else val=sqlite3_value_blob(m);\n  }",
    );
    replace(
        "  int val = sqlite3_value_bytes( columnMem(pStmt,i) );",
        "  Mem *m=columnMem(pStmt,i);\n  int val = (boxdbKind(m) || boxdbIsEscapedBlob(m)) && !(m->flags & MEM_BoxdbText) ? m->n : sqlite3_value_bytes(m);",
    );
    replace(
        "  if( affinity>=SQLITE_AFF_NUMERIC ){\n    assert( affinity==SQLITE_AFF_INTEGER",
        "  if( boxdbNativeAffinityKind(affinity) ){ boxdbNativeCoerce(pRec,boxdbNativeAffinityKind(affinity)); return; }\n  if( boxdbNativeKind(pRec) ){ if( affinity==SQLITE_AFF_TEXT ) boxdbNativeText(pRec); return; }\n  if( affinity==SQLITE_AFF_DECIMAL ){\n    boxdbDecimalCoerce(pRec);\n    return;\n  }\n  if( boxdbIsDecimal(pRec) ){ boxdbDecimalAffinity(pRec,affinity,enc); return; }\n  if( affinity>=SQLITE_AFF_NUMERIC ){\n    assert( affinity==SQLITE_AFF_INTEGER",
    );
    replace(
        "  assert( pOp->p2>=SQLITE_AFF_BLOB && pOp->p2<=SQLITE_AFF_REAL );",
        "  assert( pOp->p2>=SQLITE_AFF_BLOB && pOp->p2<=SQLITE_AFF_INTERVAL );",
    );
    replace(
        "  if( (type1 & type2 & MEM_Int)!=0 ){\nint_math:",
        "  if( ((type1|type2)&MEM_Null)==0 && (boxdbNativeKind(pIn1) || boxdbNativeKind(pIn2)) ){\n    char *error=0;\n    int op=pOp->opcode==OP_Add ? 1 : pOp->opcode==OP_Subtract ? 2 : pOp->opcode==OP_Multiply ? 3 : pOp->opcode==OP_Divide ? 4 : 5;\n    rc=boxdbNativeOp(pOut,op,0,pIn2,pIn1,&error);\n    if( rc ){ sqlite3VdbeError(p,\"%s\",error ? error : \"native allocation failed\"); sqlite3_free(error); goto abort_due_to_error; }\n  }else if( ((type1|type2)&MEM_Null)==0 && (boxdbIsDecimal(pIn1) || boxdbIsDecimal(pIn2)) ){\n    char *error = 0;\n    int op = pOp->opcode==OP_Add ? 1 : pOp->opcode==OP_Subtract ? 2 : pOp->opcode==OP_Multiply ? 3 : pOp->opcode==OP_Divide ? 4 : 5;\n    rc = boxdbDecimalOp(pOut, op, pIn2, pIn1, &error);\n    if( rc ){\n      sqlite3VdbeError(p, \"%s\", error ? error : \"DECIMAL allocation failed\");\n      sqlite3_free(error);\n      goto abort_due_to_error;\n    }\n  }else if( (type1 & type2 & MEM_Int)!=0 ){\nint_math:",
    );
    replace(
        "  int i = sqlite3_value_type(argv[0]) - 1;\n  UNUSED_PARAMETER(NotUsed);",
        "  int i = sqlite3_value_type(argv[0]) - 1;\n  if( boxdbKind((Mem*)argv[0]) ){\n    int kind=boxdbKind((Mem*)argv[0]);\n    const char *name=boxdbType(kind)->name;\n    char lower[24]; int j; for(j=0; name[j]; j++) lower[j]=sqlite3Tolower(name[j]); lower[j]=0;\n    sqlite3_result_text(context, lower, -1, SQLITE_TRANSIENT);\n    return;\n  }\n  UNUSED_PARAMETER(NotUsed);",
    );
    replace(
        "SQLITE_PRIVATE char sqlite3AffinityType(const char *zIn, Column *pCol){",
        "SQLITE_PRIVATE char sqlite3AffinityType(const char *zIn, Column *pCol){\n  while( sqlite3Isspace(*zIn) ) zIn++;\n  { unsigned i; for(i=1;i<ArraySize(boxdbTypes);i++){ int n=sqlite3Strlen30(boxdbTypes[i].name); if( sqlite3_strnicmp(zIn,boxdbTypes[i].name,n)==0 && (!zIn[n] || zIn[n]=='(' || sqlite3Isspace(zIn[n])) ) return boxdbTypes[i].affinity; } }\n  if( sqlite3_strnicmp(zIn, \"DECIMAL\", 7)==0 && (zIn[7]==0 || zIn[7]=='(' || sqlite3Isspace(zIn[7])) ) return SQLITE_AFF_DECIMAL;",
    );
    replace(
        "  combined_flags = f1|f2;\n  assert( !sqlite3VdbeMemIsRowSet",
        "  combined_flags = f1|f2;\n  if( !(combined_flags&MEM_Null) && (boxdbNativeKind(pMem1) || boxdbNativeKind(pMem2)) ){\n    int a=boxdbNativeKind(pMem1), b=boxdbNativeKind(pMem2);\n    if( a && (a==b || ((a==4 || a==5) && (b==4 || b==5))) ) return boxdbNativeCompare(pMem1,pMem2);\n    return boxdbRank(pMem1)-boxdbRank(pMem2);\n  }\n  if( (combined_flags&MEM_Null)==0 && (boxdbIsDecimal(pMem1) || boxdbIsDecimal(pMem2)) ){\n    int n1 = boxdbIsDecimal(pMem1) || (f1&(MEM_Int|MEM_IntReal|MEM_Real));\n    int n2 = boxdbIsDecimal(pMem2) || (f2&(MEM_Int|MEM_IntReal|MEM_Real));\n    if( n1 && n2 ) return boxdbDecimalCompare(pMem1, pMem2);\n    return n1 ? -1 : 1;\n  }\n  assert( !sqlite3VdbeMemIsRowSet",
    );
    replace(
        "    /* RHS is an integer */\n    if( pRhs->flags & (MEM_Int|MEM_IntReal) ){",
        "    getVarint32(aKey1+idx1, serial_type);\n    if( boxdbKind(pRhs) || boxdbIsEscapedBlob(pRhs) || (serial_type>=12 && (serial_type&1)==0 && sqlite3VdbeSerialTypeLen(serial_type)>BOXDB_DECIMAL_PREFIX_LEN && (u64)d1+sqlite3VdbeSerialTypeLen(serial_type)<=(u64)nKey1 && memcmp(aKey1+d1, \"\\000BD\", 3)==0) ){\n      if( (u64)d1+sqlite3VdbeSerialTypeLen(serial_type)>(u64)nKey1 ){ pPKey2->errCode=SQLITE_CORRUPT; return 0; }\n      mem1.db = pPKey2->pKeyInfo->db;\n      mem1.enc = pPKey2->pKeyInfo->enc;\n      sqlite3VdbeSerialGet(aKey1+d1, serial_type, &mem1);\n      rc = sqlite3MemCompare(&mem1, pRhs, pPKey2->pKeyInfo->aColl[i]);\n    }else\n    /* RHS is an integer */\n    if( pRhs->flags & (MEM_Int|MEM_IntReal) ){",
    );
    replace(
        "    affinity = pOp->p5 & SQLITE_AFF_MASK;\n    if( affinity>=SQLITE_AFF_NUMERIC ){",
        "    affinity = pOp->p5 & SQLITE_AFF_MASK;\n    if( affinity==SQLITE_AFF_DECIMAL || boxdbNativeAffinityKind(affinity) ){\n      /* Coerce temporary copies during comparison below. */\n    }else if( affinity>=SQLITE_AFF_NUMERIC ){",
    );
    replace(
        "  char affinity;      /* Affinity to use for comparison */",
        "  char affinity = SQLITE_AFF_NONE; /* Affinity to use for comparison */",
    );
    replace(
        "    res = pPKey2->r2;      /* (pKey1/nKey1) is a blob */",
        "    if( (serial_type-12)/2>5 && (u64)aKey1[0]+(serial_type-12)/2<=(u64)nKey1 && memcmp(aKey1+aKey1[0],\"\\000BD\",3)==0 ) return sqlite3VdbeRecordCompare(nKey1,pKey1,pPKey2);\n    res = pPKey2->r2;      /* (pKey1/nKey1) is a blob */",
    );
    replace(
        "  u8 ovrfl;         /* Integer overflow seen */\n};",
        "  u8 ovrfl;         /* Integer overflow seen */\n  int decimalMode;  /* A decimal input has promoted SUM/AVG. */\n  Mem decimalSum;   /* Also preserves an integer overflow until promotion. */\n};\n#include \"boxdb_aggregate.inc\"",
    );
    replace(
        "  if( p && type!=SQLITE_NULL ){\n    p->cnt++;",
        "  if( boxdbSumStep(context,p,(Mem*)argv[0],type) ) return;\n  if( p && type!=SQLITE_NULL ){\n    p->cnt++;",
    );
    replace(
        "  if( ALWAYS(p) && type!=SQLITE_NULL ){\n    assert( p->cnt>0 );",
        "  if( boxdbSumInverse(context,p,(Mem*)argv[0],type) ) return;\n  if( ALWAYS(p) && type!=SQLITE_NULL ){\n    assert( p->cnt>0 );",
    );
    replace(
        "          p->ovrfl = 1;\n          kahanBabuskaNeumaierInit(p, p->iSum);",
        "          if( strcmp(context->pFunc->zName,\"total\")!=0 && boxdbSumUpdate(context,p,(Mem*)argv[0],1)!=SQLITE_OK ) return;\n          p->ovrfl = 1;\n          kahanBabuskaNeumaierInit(p, p->iSum);",
    );
    replace(
        "static void sumFinalize(sqlite3_context *context){\n  SumCtx *p;\n  p = sqlite3_aggregate_context(context, 0);",
        "static void sumValue(sqlite3_context *context){\n  SumCtx *p;\n  p = sqlite3_aggregate_context(context, 0);\n  if( p && p->decimalMode && boxdbKind(&p->decimalSum) ){\n    if( p->cnt>0 ) sqlite3_result_value(context, &p->decimalSum);\n    return;\n  }",
    );
    replace(
        "static void avgFinalize(sqlite3_context *context){\n  SumCtx *p;\n  p = sqlite3_aggregate_context(context, 0);",
        "static void sumFinalize(sqlite3_context *context){ sumValue(context); boxdbSumRelease(context); }\nstatic void avgValue(sqlite3_context *context){\n  SumCtx *p;\n  p = sqlite3_aggregate_context(context, 0);\n  if( p && p->decimalMode && boxdbKind(&p->decimalSum) ){\n    if( p->cnt>0 ) boxdbAverage(context,p);\n    return;\n  }",
    );
    replace(
        "static void totalFinalize(sqlite3_context *context){",
        "static void avgFinalize(sqlite3_context *context){ avgValue(context); boxdbSumRelease(context); }\nstatic void totalValue(sqlite3_context *context){",
    );
    replace(
        "  double r = 0.0;\n  p = sqlite3_aggregate_context(context, 0);\n  if( p ){",
        "  double r = 0.0;\n  p = sqlite3_aggregate_context(context, 0);\n  if( p && p->decimalMode && boxdbKind(&p->decimalSum) ){\n    sqlite3AtoF(p->decimalSum.z+BOXDB_DECIMAL_PREFIX_LEN,&r);\n    sqlite3_result_double(context,r);\n    return;\n  }\n  if( p ){",
    );
    replace(
        "typedef struct CountCtx CountCtx;",
        "static void totalFinalize(sqlite3_context *context){ totalValue(context); boxdbSumRelease(context); }\ntypedef struct CountCtx CountCtx;",
    );
    replace(
        "sumStep, sumFinalize, sumFinalize, sumInverse",
        "sumStep, sumFinalize, sumValue, sumInverse",
    );
    replace(
        "sumStep, avgFinalize, avgFinalize, sumInverse",
        "sumStep, avgFinalize, avgValue, sumInverse",
    );
    replace(
        "sumStep,totalFinalize,totalFinalize,sumInverse",
        "sumStep,totalFinalize,totalValue,sumInverse",
    );
    replace(
        "  if( (flags1 & (MEM_Str|MEM_Blob))==0 ){",
        "  if( boxdbKind(pIn1) || boxdbKind(pIn2) || boxdbIsEscapedBlob(pIn1) || boxdbIsEscapedBlob(pIn2) ){\n    rc=boxdbConcat(pOut,pIn2,pIn1,encoding);\n    if( rc ) goto abort_due_to_error;\n    break;\n  }\n  if( (flags1 & (MEM_Str|MEM_Blob))==0 ){",
    );
    replace(
        "SQLITE_API int sqlite3_value_bytes16(sqlite3_value *pVal){",
        "SQLITE_API int sqlite3_value_bytes16(sqlite3_value *pVal){\n  if( boxdbKind(pVal) || boxdbIsEscapedBlob(pVal) ){\n    if( !boxdbValueText(pVal,SQLITE_UTF16NATIVE) ) return 0;\n    return -pVal->u.nZero-1;\n  }",
    );
    replace(
        "  int val = sqlite3_value_bytes16( columnMem(pStmt,i) );",
        "  Mem *m=columnMem(pStmt,i);\n  int val=(boxdbKind(m) || boxdbIsEscapedBlob(m)) && !(m->flags & MEM_BoxdbText) ? m->n : sqlite3_value_bytes16(m);",
    );
    replace(
        "  setResultStrOrError(pCtx, z, n, 0, xDel);",
        "  setResultStrOrError(pCtx, z, n, 0, xDel);\n  if( !(pCtx->pOut->flags & MEM_Null) && boxdbEscapeBlob(pCtx->pOut) ) sqlite3_result_error_nomem(pCtx);",
    );
    replace(
        "    setResultStrOrError(pCtx, z, (int)n, 0, xDel);",
        "    setResultStrOrError(pCtx, z, (int)n, 0, xDel);\n    if( !(pCtx->pOut->flags & MEM_Null) && boxdbEscapeBlob(pCtx->pOut) ) sqlite3_result_error_nomem(pCtx);",
    );
    replace(
        "        if( encoding==0 ) pVar->enc = ENC(p->db);",
        "        if( encoding==0 ){ pVar->enc=ENC(p->db); if( rc==SQLITE_OK ) rc=boxdbEscapeBlob(pVar); }",
    );
    replace(
        "    sqlite3VdbeMemSetPointer(&p->aVar[i-1], pPtr, zPTtype, xDestructor);",
        "    if( zPTtype && strcmp(zPTtype,\"boxdb.wire-blob.v1\")==0 ){\n      const BoxdbWireBlob *blob=(const BoxdbWireBlob*)pPtr;\n      rc=sqlite3VdbeMemSetStr(&p->aVar[i-1],blob->data,blob->length,0,SQLITE_TRANSIENT);\n      if( xDestructor ) xDestructor(pPtr);\n    }else sqlite3VdbeMemSetPointer(&p->aVar[i-1], pPtr, zPTtype, xDestructor);",
    );
    replace(
        "  pOut = pCtx->pOut;\n  assert( sqlite3_mutex_held(pOut->db->mutex) );\n  sqlite3VdbeMemRelease(pOut);",
        "  pOut = pCtx->pOut;\n  assert( sqlite3_mutex_held(pOut->db->mutex) );\n  if( zPType && strcmp(zPType,\"boxdb.wire-blob.v1\")==0 ){\n    const BoxdbWireBlob *blob=(const BoxdbWireBlob*)pPtr;\n    if( sqlite3VdbeMemSetStr(pOut,blob->data,blob->length,0,SQLITE_TRANSIENT) ) sqlite3_result_error_nomem(pCtx);\n    if( xDestructor ) xDestructor(pPtr);\n    return;\n  }\n  sqlite3VdbeMemRelease(pOut);",
    );
    replace(
        "        rc = sqlite3_bind_blob(pStmt, i, pValue->z, pValue->n,SQLITE_TRANSIENT);",
        "        if( boxdbKind(pValue) || boxdbIsEscapedBlob(pValue) ){\n          BoxdbWireBlob wire={pValue->z,pValue->n};\n          rc=sqlite3_bind_pointer(pStmt,i,&wire,\"boxdb.wire-blob.v1\",0);\n        }else rc = sqlite3_bind_blob(pStmt, i, pValue->z, pValue->n,SQLITE_TRANSIENT);",
    );
    replace(
        "    res = sqlite3MemCompare(pIn3, pIn1, pOp->p4.pColl);",
        "    res = (boxdbNativeAffinityKind(affinity) || (affinity==SQLITE_AFF_TEXT && (boxdbNativeKind(pIn1) || boxdbNativeKind(pIn3)))) ? boxdbNativeAffinityCompare(pIn3,pIn1,pOp->p4.pColl,affinity) : (affinity==SQLITE_AFF_DECIMAL || (affinity==SQLITE_AFF_TEXT && (boxdbIsDecimal(pIn1) || boxdbIsDecimal(pIn3)))) ? boxdbDecimalAffinityCompare(pIn3,pIn1,pOp->p4.pColl,affinity) : sqlite3MemCompare(pIn3, pIn1, pOp->p4.pColl);",
    );
    replace(
        "  char aff1 = sqlite3ExprAffinity(pExpr);",
        "  char aff1 = sqlite3ExprAffinity(pExpr);\n  if( boxdbNativeAffinityKind(aff1) ) return aff1;\n  if( boxdbNativeAffinityKind(aff2) ) return aff2;\n  if( aff1==SQLITE_AFF_DECIMAL || aff2==SQLITE_AFF_DECIMAL ) return SQLITE_AFF_DECIMAL;",
    );
    replace(
        "                              \"real\", \"flexnum\" };",
        "                              \"real\", \"flexnum\", \"decimal\", \"uuid\", \"instant\", \"date\", \"datetime\", \"interval\" };",
    );
    replace(
        "(aff>=SQLITE_AFF_BLOB && aff<=SQLITE_AFF_FLEXNUM)",
        "(aff>=SQLITE_AFF_BLOB && aff<=SQLITE_AFF_INTERVAL)",
    );
    replace(
        "    if( aff>SQLITE_AFF_NUMERIC) aff = SQLITE_AFF_NUMERIC;",
        "    if( aff>SQLITE_AFF_NUMERIC && aff!=SQLITE_AFF_DECIMAL && !boxdbNativeAffinityKind(aff) ) aff = SQLITE_AFF_NUMERIC;",
    );
    replace(
        "    const Mem *p = &aMem[i];\n    if( p->flags & (MEM_Int|MEM_IntReal) ){",
        "    const Mem *p = &aMem[i];\n    if( boxdbNativeKind(p) ){\n      h+=boxdb_native_hash(boxdbNativeKind(p),p->z+5,p->n-5);\n    }else if( boxdbIsDecimal(p) ){\n      h+=boxdbDecimalInt(p);\n    }else if( p->flags & (MEM_Int|MEM_IntReal) ){",
    );
    replace(
        "  static FuncDef aBuiltinFunc[] = {",
        "  static FuncDef aBuiltinFunc[] = {\n    FUNCTION(__boxdb_decimal_valid,3,0,0,boxdbDecimalValidFunc),\n    FUNCTION(extract,2,1,0,boxdbTemporalFunction),\n    FUNCTION(make_interval,-1,2,0,boxdbTemporalFunction),\n    FUNCTION(age,2,3,0,boxdbTemporalFunction),\n    VFUNCTION(age,1,5,0,boxdbTemporalFunction),\n    FUNCTION(date_trunc,2,4,0,boxdbTemporalFunction),",
    );
    replace(
        "  iA = sqlite3VdbeIntValue(pIn2);\n  iB = sqlite3VdbeIntValue(pIn1);",
        "  if( boxdbNativeKind(pIn1) || boxdbNativeKind(pIn2) ){sqlite3VdbeError(p,\"bitwise operators require integers\");rc=SQLITE_ERROR;goto abort_due_to_error;}\n  iA = sqlite3VdbeIntValue(pIn2);\n  iB = sqlite3VdbeIntValue(pIn1);",
    );
    replace(
        "    pOut->u.i = ~sqlite3VdbeIntValue(pIn1);",
        "    if( boxdbNativeKind(pIn1) ){sqlite3VdbeError(p,\"bitwise operators require integers\");rc=SQLITE_ERROR;goto abort_due_to_error;}\n    pOut->u.i = ~sqlite3VdbeIntValue(pIn1);",
    );
    replace("  n += 35 + 6*p->nCol;", "  n += 35 + 12*p->nCol;");
    replace(
        "#define SQLITE_AFF_MASK     0x47",
        "#define SQLITE_AFF_MASK     0x4f",
    );
    replace(
        "#define sqlite3IsNumericAffinity(X)  ((X)>=SQLITE_AFF_NUMERIC)",
        "#define sqlite3IsNumericAffinity(X)  ((X)>=SQLITE_AFF_NUMERIC && (X)<=SQLITE_AFF_DECIMAL)",
    );
    assert_eq!(
        source.matches("pCol->affinity>=SQLITE_AFF_NUMERIC").count(),
        3
    );
    source = source.replace(
        "pCol->affinity>=SQLITE_AFF_NUMERIC",
        "sqlite3IsNumericAffinity(pCol->affinity)",
    );
    let path = PathBuf::from(out_dir).join("sqlite3-boxdb.c");
    fs::write(&path, source).expect("write patched SQLite");
    path
}
