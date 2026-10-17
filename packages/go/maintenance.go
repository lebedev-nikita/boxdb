package boxdb

import "strings"

type CheckMode string

const (
	CheckQuick CheckMode = "quick"
	CheckFull  CheckMode = "full"
)

type CheckStatus string

const (
	CheckValid      CheckStatus = "valid"
	CheckInvalid    CheckStatus = "invalid"
	CheckIncomplete CheckStatus = "incomplete"
)

type Check struct {
	Name   string      `json:"name"`
	Status CheckStatus `json:"status"`
}
type CheckIssue struct {
	Check    string  `json:"check"`
	Code     string  `json:"code"`
	Message  string  `json:"message"`
	Table    *string `json:"table"`
	Column   *string `json:"column"`
	RowID    *string `json:"rowid"`
	Document *int    `json:"document"`
	Line     *int    `json:"line"`
}
type CheckReport struct {
	Status   CheckStatus  `json:"status"`
	Complete bool         `json:"complete"`
	Format   *Format      `json:"format"`
	Mode     CheckMode    `json:"mode"`
	Checks   []Check      `json:"checks"`
	Issues   []CheckIssue `json:"issues"`
}
type RejectedDocument struct {
	Document int    `json:"document"`
	Line     int    `json:"line"`
	Code     string `json:"code"`
	Message  string `json:"message"`
}
type RecoveryReport struct {
	Output        CheckReport        `json:"output"`
	RecoveredRows uint64             `json:"recovered_rows"`
	LossPossible  bool               `json:"loss_possible"`
	Rejected      []RejectedDocument `json:"rejected"`
	RejectedPath  *string            `json:"rejected_path"`
	Warnings      []string           `json:"warnings"`
}
type fileCommand struct {
	Operation   string  `json:"operation"`
	Source      string  `json:"source"`
	Destination *string `json:"destination"`
	Mode        string  `json:"mode"`
}

func fileMaintenance(operation, source string, destination *string, mode string, out any) error {
	if strings.ContainsRune(source, 0) || destination != nil && strings.ContainsRune(*destination, 0) {
		return validationError("path contains NUL")
	}
	return nativeMaintenance(fileCommand{operation, source, destination, mode}, out)
}

// CheckFile checks an existing file without opening a writer. Invalid data is
// returned in the report; errors indicate that the operation could not run.
func CheckFile(path string, mode CheckMode) (*CheckReport, error) {
	var report CheckReport
	if mode != CheckQuick && mode != CheckFull {
		return nil, validationError("mode must be quick or full")
	}
	err := fileMaintenance("check", path, nil, string(mode), &report)
	return &report, err
}

// RepairFile rebuilds indexes from readable tables into a new binary file.
func RepairFile(source, destination string) (*RecoveryReport, error) {
	var report RecoveryReport
	err := fileMaintenance("repair", source, &destination, "binary", &report)
	return &report, err
}

// RecoverFile salvages available data into a new file, without guaranteeing completeness.
func RecoverFile(source, destination string, format Format) (*RecoveryReport, error) {
	if format != Binary && format != Readable {
		return nil, validationError("format must be binary or readable")
	}
	var report RecoveryReport
	err := fileMaintenance("recover", source, &destination, string(format), &report)
	return &report, err
}
