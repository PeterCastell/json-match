
The user will specify several match sets which list fieldMatches

Each FieldMatch specifies the full path through json objects to get there, what type they must be or an exact string they must be or any type, an optional regex predicate, and whether or not they should be captured. For a MatchSet to match, all FieldMatches need to be satisfied.

Each FieldMatch is capable of capturing the value of that field. A FieldMatch can capture an object while another FieldMatch can capture a child of that object.
The predicate of a FieldMatch is capable of capturing the values of named capture groups.

Each captured value will be either stored as a byte range within the original string, or parsed into a value.

Multiple MatchSets will be compiled into a single MatchMachine. This should be fully optimized for traversing a string and only capturing the fields desired. Do not bother parsing any fields or values that are not wanted. The operation of a MatchMachine needs to be as fast as possible.

A match operation will write the captures into a MachineResult, storing captures by machine index, and whether each set matched.