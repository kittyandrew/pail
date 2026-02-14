-- Add optional description field to sources (user-provided context about the source)
ALTER TABLE sources ADD COLUMN description TEXT;
