static int boxdbSumUpdate(sqlite3_context *ctx, SumCtx *p, Mem *value, int op){
  char *error = 0;
  int rc;
  if( p->decimalMode==6 ){
    rc=boxdbNativeOp(&p->decimalSum,op,0,&p->decimalSum,value,&error);
    if( rc ){sqlite3_result_error(ctx,error ? error : "INTERVAL allocation failed",-1);sqlite3_free(error);}
    return rc;
  }
  if( p->approx && !p->ovrfl ){
    sqlite3_result_error(ctx, "DECIMAL aggregate requires an explicit REAL conversion", -1);
    return SQLITE_ERROR;
  }
  if( !boxdbIsDecimal(&p->decimalSum) ){
    p->decimalSum.db = sqlite3_context_db_handle(ctx);
    p->decimalSum.flags = MEM_Int;
    p->decimalSum.u.i = p->iSum;
  }
  rc = boxdbDecimalOp(&p->decimalSum, op, &p->decimalSum, value, &error);
  if( rc ){
    sqlite3_result_error(ctx, error ? error : "DECIMAL allocation failed", -1);
    sqlite3_free(error);
  }
  return rc;
}

/* Retain an exact overflow accumulator until a decimal promotes SUM/AVG.
** Pure integer aggregates still report SQLite's integer-overflow error. */
static int boxdbSumStep(sqlite3_context *ctx, SumCtx *p, Mem *value, int type){
  int kind=boxdbNativeKind(value);
  if( !p || type==SQLITE_NULL ) return 0;
  if( kind || p->decimalMode==6 ){
    if( kind!=6 || strcmp(ctx->pFunc->zName,"total")==0 || (p->cnt && p->decimalMode!=6) ){
      sqlite3_result_error(ctx,"aggregate requires numeric inputs or homogeneous INTERVAL values",-1); return 1;
    }
    if( p->decimalMode!=6 ){
      static const char zero[17]="v";
      p->decimalSum.db=sqlite3_context_db_handle(ctx);
      if( sqlite3VdbeMemSetStr(&p->decimalSum,zero,17,0,SQLITE_STATIC) ){ sqlite3_result_error_nomem(ctx);return 1; }
      p->decimalMode=6;
    }
    if( boxdbSumUpdate(ctx,p,value,1)==SQLITE_OK ) p->cnt++;
    return 1;
  }
  if( strcmp(ctx->pFunc->zName,"total")==0 ) return 0;
  if( boxdbIsDecimal(value) || p->decimalMode ){
    p->decimalMode=1;
    if( boxdbSumUpdate(ctx,p,value,1)==SQLITE_OK ){
      p->cnt++;
      p->approx=0;
      p->ovrfl=0;
    }
    return 1;
  }
  if( boxdbIsDecimal(&p->decimalSum) ){
    if( type==SQLITE_INTEGER ){
      if( boxdbSumUpdate(ctx,p,value,1)!=SQLITE_OK ) return 1;
    }else{
      sqlite3VdbeMemSetNull(&p->decimalSum);
    }
  }
  return 0;
}

static int boxdbSumInverse(sqlite3_context *ctx, SumCtx *p, Mem *value, int type){
  if( !p || type==SQLITE_NULL || !boxdbKind(&p->decimalSum) ) return 0;
  if( boxdbSumUpdate(ctx,p,value,2)!=SQLITE_OK ) return 1;
  if( p->decimalMode ){ p->cnt--; return 1; }
  return 0;
}

static void boxdbAverage(sqlite3_context *ctx, SumCtx *p){
  Mem count, result;
  char *error = 0;
  memset(&count, 0, sizeof(count));
  memset(&result, 0, sizeof(result));
  count.flags = MEM_Int;
  count.u.i = p->cnt;
  result.db = sqlite3_context_db_handle(ctx);
  if( (p->decimalMode==6 ? boxdbNativeOp(&result,4,0,&p->decimalSum,&count,&error) : boxdbDecimalOp(&result, 4, &p->decimalSum, &count, &error)) ){
    sqlite3_result_error(ctx, error ? error : "DECIMAL allocation failed", -1);
    sqlite3_free(error);
  }else{
    sqlite3_result_value(ctx, &result);
  }
  sqlite3VdbeMemRelease(&result);
}

static void boxdbSumRelease(sqlite3_context *ctx){
  SumCtx *p = sqlite3_aggregate_context(ctx, 0);
  if( p ) sqlite3VdbeMemRelease(&p->decimalSum);
}
