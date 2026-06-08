#ifndef QMVIR_QM_API_H
#define QMVIR_QM_API_H

#ifdef __cplusplus
extern "C" {
#endif

// Public function signatures only (no implementation logic).
int qm_init(const char* data_dir);
int qm_exec_sql(const char* sql, char* out_buf, unsigned long out_cap);
int qm_backup(const char* path);
int qm_restore(const char* path);
void qm_shutdown(void);

#ifdef __cplusplus
}
#endif

#endif
